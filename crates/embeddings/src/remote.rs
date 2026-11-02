//! A URL mode: an OpenAI-compatible `POST <url>/embeddings`, one round trip per text.
//!
//! The endpoint is the operator's explicit setting, because a URL sends every note and
//! every skill's text to whoever answers it. Nothing is retried here: a failure falls
//! back to the lexical score, and the caller says so once a session rather than once a
//! round.

use letibot_http::{Endpoint, HttpError, Request};

/// The path under the URL, so `mode = "http://host:8080"` and
/// `mode = "http://host:8080/v1"` are the same endpoint spelled two ways.
const ROUTE: &str = "embeddings";

/// Why an endpoint could not be built or did not answer.
#[derive(Debug)]
pub enum RemoteError {
    /// The URL is not `http://HOST[:PORT][/prefix]`.
    Url(String),
    /// No answer, or an answer that is not HTTP.
    Http(String),
    /// The server answered, and not with 2xx. The body is kept: an endpoint's own
    /// complaint is more use than "the request failed".
    Refused { status: u16, body: String },
    /// A 2xx whose body is not this API's shape.
    Malformed(String),
    /// Any of the above, and which endpoint said it: a box with two of them written in a
    /// file needs the one that failed named.
    At { url: String, why: String },
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteError::Url(u) => write!(
                f,
                "{u} is not a URL this client can reach: it speaks http://HOST[:PORT][/prefix] \
                 with no TLS"
            ),
            RemoteError::Http(e) => write!(f, "the embeddings endpoint: {e}"),
            RemoteError::Refused { status, body } => write!(
                f,
                "the embeddings endpoint answered {status}: {}",
                body.trim()
            ),
            RemoteError::Malformed(why) => {
                write!(
                    f,
                    "the embeddings endpoint answered a body this API does not: {why}"
                )
            }
            RemoteError::At { url, why } => write!(f, "{url}: {why}"),
        }
    }
}

impl std::error::Error for RemoteError {}

impl From<HttpError> for RemoteError {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Status { code, body } => RemoteError::Refused { status: code, body },
            other => RemoteError::Http(other.to_string()),
        }
    }
}

/// One endpoint, and the model name it answers to.
#[derive(Debug, Clone)]
pub struct Remote {
    endpoint: Endpoint,
    /// Everything after the authority, with no trailing slash, or empty.
    prefix: String,
    model: String,
}

impl Remote {
    /// Build from the URL the config wrote and the model name beside it.
    pub fn open(url: &str, model: &str) -> Result<Remote, RemoteError> {
        let rest = url
            .trim()
            .strip_prefix("http://")
            .ok_or_else(|| RemoteError::Url(url.to_string()))?;
        let (authority, prefix) = match rest.find('/') {
            Some(at) => (&rest[..at], rest[at..].trim_end_matches('/')),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(RemoteError::Url(url.to_string()));
        }
        // A port is optional and 80 is what an absent one means; `Endpoint::parse`
        // wants it spelled, so it is spelled here.
        let endpoint = if authority.contains(':') {
            Endpoint::parse(authority).map_err(|e| RemoteError::Url(format!("{url}: {e}")))?
        } else {
            Endpoint::new(authority, 80)
        };
        Ok(Remote {
            endpoint,
            prefix: prefix.to_string(),
            model: model.trim().to_string(),
        })
    }

    /// The name a cache key is built from: the model the endpoint was asked for.
    pub fn name(&self) -> &str {
        &self.model
    }

    /// The path this endpoint is posted to.
    pub fn path(&self) -> String {
        // `/v1` is the documented prefix of an OpenAI-compatible server, so a URL that
        // names no path at all still reaches the route.
        let prefix = if self.prefix.is_empty() {
            "/v1"
        } else {
            self.prefix.as_str()
        };
        format!("{prefix}/{ROUTE}")
    }

    /// The URL as the config wrote it, for a sentence that has to name one endpoint.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.endpoint.authority(), self.prefix)
    }

    /// One vector per text, in the order they were handed over.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, RemoteError> {
        self.ask(texts).map_err(|e| RemoteError::At {
            url: self.url(),
            why: e.to_string(),
        })
    }

    fn ask(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, RemoteError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({ "model": self.model, "input": texts }).to_string();
        let path = self.path();
        let request =
            Request::post_json(&path, &body).with_headers(&[("Accept", "application/json")]);
        let answer = letibot_http::send(&self.endpoint, request)?.read_to_string()?;
        parse(&answer, texts.len())
    }
}

/// The API's answer, in the order the request asked for: `data[i].embedding`, sorted by
/// `index` because a server is free to answer out of order.
fn parse(answer: &str, wanted: usize) -> Result<Vec<Vec<f32>>, RemoteError> {
    let value: serde_json::Value = serde_json::from_str(answer)
        .map_err(|e| RemoteError::Malformed(format!("not JSON: {e}")))?;
    let data = value
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| RemoteError::Malformed("there is no `data` array".into()))?;
    let mut rows: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
    for row in data {
        let index = row
            .get("index")
            .and_then(|i| i.as_u64())
            .ok_or_else(|| RemoteError::Malformed("a row has no `index`".into()))?;
        let embedding = row
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(|| RemoteError::Malformed(format!("row {index} has no `embedding`")))?;
        let mut vector = Vec::with_capacity(embedding.len());
        for x in embedding {
            vector.push(
                x.as_f64().ok_or_else(|| {
                    RemoteError::Malformed(format!("row {index} holds a non-number"))
                })? as f32,
            );
        }
        if vector.is_empty() {
            return Err(RemoteError::Malformed(format!("row {index} is empty")));
        }
        rows.push((index, vector));
    }
    if rows.len() != wanted {
        return Err(RemoteError::Malformed(format!(
            "{wanted} texts went out and {} vectors came back",
            rows.len()
        )));
    }
    rows.sort_by_key(|(i, _)| *i);
    let dim = rows[0].1.len();
    if let Some((i, v)) = rows.iter().find(|(_, v)| v.len() != dim) {
        return Err(RemoteError::Malformed(format!(
            "row {i} is {} wide and the first is {dim}",
            v.len()
        )));
    }
    Ok(rows.into_iter().map(|(_, v)| v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// One loopback answer, on a port the kernel picks. The server is the test's own and
    /// dies with it, so this measures the client and never the box's network.
    struct Fake {
        port: u16,
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Fake {
        fn new(status: &'static str, body: &'static str) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut head = String::new();
                    let mut length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        if let Some(n) = line.strip_prefix("content-length: ") {
                            length = n.trim().parse().unwrap_or(0);
                        } else if let Some(n) =
                            line.to_ascii_lowercase().strip_prefix("content-length: ")
                        {
                            length = n.trim().parse().unwrap_or(0);
                        }
                        head.push_str(&line);
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let mut payload = vec![0u8; length];
                    if length > 0 {
                        use std::io::Read;
                        let _ = reader.read_exact(&mut payload);
                    }
                    log.lock()
                        .expect("log")
                        .push(format!("{head}{}", String::from_utf8_lossy(&payload)));
                    let mut out = stream;
                    let answer = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = out.write_all(answer.as_bytes());
                    let _ = out.flush();
                }
            });
            Fake { port, seen }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        fn request(&self) -> String {
            let seen = self.seen.lock().expect("log");
            seen.first().cloned().unwrap_or_default()
        }
    }

    /// The route, the model name and the texts: the three things an OpenAI-compatible
    /// server needs, and the path `/v1/embeddings` when the URL names only a host.
    #[test]
    fn a_url_mode_posts_the_texts_and_reads_the_vectors_back() {
        let fake = Fake::new(
            "200 OK",
            r#"{"data":[{"index":1,"embedding":[3.0,4.0]},{"index":0,"embedding":[1.0,0.0]}]}"#,
        );
        let remote = Remote::open(&fake.url(), "bge-m3").expect("open");
        let vectors = remote.embed(&["one", "two"]).expect("embed");
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![3.0, 4.0]]);
        let sent = fake.request();
        assert!(sent.starts_with("POST /v1/embeddings HTTP/1.1"), "{sent}");
        assert!(sent.contains("\"model\":\"bge-m3\""), "{sent}");
        assert!(sent.contains("\"input\":[\"one\",\"two\"]"), "{sent}");
    }

    /// A URL that names a path keeps it, so a server behind a prefix is reachable without
    /// a second key.
    #[test]
    fn a_url_with_a_prefix_posts_under_it() {
        assert_eq!(
            Remote::open("http://host:9000/v1", "m").unwrap().path(),
            "/v1/embeddings"
        );
        assert_eq!(
            Remote::open("http://host:9000/openai/v1/", "m")
                .unwrap()
                .path(),
            "/openai/v1/embeddings"
        );
        assert_eq!(
            Remote::open("http://host", "m").unwrap().path(),
            "/v1/embeddings"
        );
    }

    /// A refusal keeps the server's own words: "the request failed" is what an operator
    /// cannot act on.
    #[test]
    fn a_refused_request_keeps_the_servers_words() {
        let fake = Fake::new("500 Internal Server Error", r#"{"error":"no such model"}"#);
        let remote = Remote::open(&fake.url(), "bge-m3").expect("open");
        let err = remote.embed(&["one"]).unwrap_err();
        let said = err.to_string();
        assert!(said.contains("500"), "{said}");
        assert!(said.contains("no such model"), "{said}");
    }

    /// A body that is not the API's shape is refused by name, so a wrong URL is a
    /// sentence rather than a zero score.
    #[test]
    fn a_body_that_is_not_this_api_is_refused_by_name() {
        let fake = Fake::new("200 OK", r#"{"vectors":[[1.0]]}"#);
        let remote = Remote::open(&fake.url(), "m").expect("open");
        let said = remote.embed(&["one"]).unwrap_err().to_string();
        assert!(said.contains("data"), "{said}");

        let short = Fake::new("200 OK", r#"{"data":[{"index":0,"embedding":[1.0]}]}"#);
        let remote = Remote::open(&short.url(), "m").expect("open");
        let said = remote.embed(&["one", "two"]).unwrap_err().to_string();
        assert!(said.contains("2 texts"), "{said}");
    }

    /// A URL this client cannot speak is refused where it is written.
    #[test]
    fn a_url_that_is_not_http_is_refused() {
        assert!(matches!(
            Remote::open("https://api.example.com", "m"),
            Err(RemoteError::Url(_))
        ));
        assert!(matches!(Remote::open("", "m"), Err(RemoteError::Url(_))));
    }
}
