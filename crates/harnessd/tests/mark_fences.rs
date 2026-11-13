//! A reply's `note`/`suggest` fences, end to end: a real turn on a fake provider, and the
//! `Citations` event a head draws a mark from.

use std::sync::{Arc, Mutex};

use letibot_harnessd::Parts;
use letibot_harnessd::config::{Config, ProviderConfig, Seat};
use letibot_harnessd::harness::Harness;
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::event::CitationKind;
use letibot_sessionlog::protocol::{
    AnswerMode, ContextAsk, ContextPos, ContextRange, ContextRef, EditorContext,
};

/// The reply: one suggestion over the unsaved lines the prompt carried, and one citation of
/// lines the buffer does NOT hold - so one mark is anchored to the buffer and one to the file.
const REPLY: &str = "Two things about the article.\n\n\
    ```suggest docs/article.md:3-4\n\
    The cable was laid in 1866 by the Great Eastern.\n\
    ```\n\n\
    and the opening, which is already saved:\n\n\
    ```docs/article.md:1-2\n```\n";

#[test]
fn a_suggest_fence_is_published_with_its_kind_its_body_and_the_buffers_lines() {
    let dir = TempDir::new("mark-fences");
    let root = dir.path();
    // Nothing of the operator's may reach this session: a key in their `providers.toml` would decide
    // where a request goes, and no request may leave loopback.
    std::fs::create_dir_all(root.join("config/letibot")).expect("config dir");
    // SAFETY: this binary has ONE test, and these are set before the daemon, any thread that reads
    // config, or the harness exists.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("GLM_API_KEY");
        std::env::remove_var("LETIBOT_MODE");
    }

    let workspace = root.join("ws");
    std::fs::create_dir_all(workspace.join("docs")).expect("workspace");
    // What is on DISK: line 3 and 4 say one thing, and the buffer below says another.
    std::fs::write(
        workspace.join("docs/article.md"),
        "An article\nabout telegraphy\nline three as saved\nline four as saved\n",
    )
    .expect("the article");
    let fake = Fake::serve(REPLY);
    std::fs::write(
        root.join("config/letibot/providers.toml"),
        format!(
            "# Written by crates/harnessd/tests/mark_fences.rs — a fake on loopback.\n\
             [deepseek]\nkey = \"sk-not-used\"\nurl = \"{}\"\n",
            fake.url
        ),
    )
    .expect("providers.toml");

    let mut cfg = Config::for_this_box(&workspace);
    cfg.dialect = letibot_harnessd::Dialect::Qwen;
    cfg.seat = Seat::Leticode;
    cfg.http_retries = 0;
    cfg.session_id = "s-mark-fences".into();
    // The byte vocabulary and a provider, so this runs on a machine with no model file at all.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    let hub = letibot_sessionlog::hub::Hub::new(cfg.session_id.clone());
    let mut h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");

    // The operator's editor, holding an unsaved article: the selection is lines 3 and 4 as the
    // BUFFER has them, which the file on disk does not.
    let file = workspace.join("docs/article.md");
    let context = EditorContext {
        origin: "rano".into(),
        refs: vec![ContextRef {
            file: file.display().to_string(),
            range: Some(ContextRange {
                start: ContextPos { line: 3, column: 1 },
                end: ContextPos { line: 5, column: 1 },
            }),
            cursor: None,
            text: "line three, edited\nline four, edited\n".into(),
            unsaved: true,
            lang: "md".into(),
            title: String::new(),
        }],
        ask: Some(ContextAsk {
            preset: "fill the gap".into(),
            answer: AnswerMode::Suggest,
        }),
        ..Default::default()
    };

    let before = hub.retained().len();
    // The door the daemon uses, not `submit_with_context` on its own: the turn tail is what brackets
    // the prompt's span, and the citations are published as the reply's row lands inside it.
    use letibot_harnessd::turn_tail::{Bare, Door, run_turn};
    run_turn(&mut h, &mut Bare, Door::Prompt, |h| {
        h.submit_with_context("fill the gap please", &[], Some(&context))
            .map(Some)
    })
    .expect("a spoken turn always runs")
    .expect("the turn runs");
    assert_eq!(fake.rounds(), 1, "one reply, one round");

    let published: Vec<Vec<letibot_sessionlog::Citation>> = hub
        .retained()
        .into_iter()
        .skip(before)
        .filter_map(|env| match env.event {
            SessionEvent::Citations { citations, .. } => Some(citations),
            _ => None,
        })
        .collect();
    assert_eq!(published.len(), 1, "one reply, one Citations event");
    let cits = &published[0];
    assert_eq!(cits.len(), 2, "a suggestion and a citation: {cits:?}");

    let suggest = &cits[0];
    assert_eq!(suggest.kind, CitationKind::Suggest, "{suggest:?}");
    assert_eq!(suggest.label, "docs/article.md:3-4");
    assert_eq!(
        (suggest.path.as_str(), suggest.start, suggest.end),
        ("docs/article.md", 3, 4)
    );
    assert_eq!(
        suggest.body, "The cable was laid in 1866 by the Great Eastern.",
        "the fence's body is the replacement the editor offers"
    );
    assert_eq!(
        suggest.lines,
        vec!["line three, edited", "line four, edited"],
        "the anchor is the BUFFER's text - the file on disk says something else"
    );
    assert_eq!(suggest.error, None);

    let cite = &cits[1];
    assert_eq!(cite.kind, CitationKind::Cite, "{cite:?}");
    assert_eq!(
        cite.lines,
        vec!["An article", "about telegraphy"],
        "a range the prompt did not carry unsaved is read from the disk, as it always was"
    );
    assert!(cite.body.is_empty(), "a citation is an empty fence");
}

/// A directory that removes itself, because a test that leaks a store per run eventually fills the
/// disk and the run that finds out is not this one.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = letibot_sessionlog::testing::scratch(tag);
        std::fs::create_dir_all(&path).expect("creating the temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A fake provider on a loopback port, answering the one reply this test is about.
struct Fake {
    url: String,
    rounds: Arc<Mutex<usize>>,
}

impl Fake {
    fn serve(reply: &'static str) -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let url = format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            listener.local_addr().unwrap().port()
        );
        let rounds = Arc::new(Mutex::new(0usize));
        let seen = Arc::clone(&rounds);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                // The request is drained and discarded: what this fake answers is the reply, and a
                // provider that answered from the conversation would be a second model.
                let _ = read_request(&conn);
                *seen.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                let _ = write_sse(conn, reply);
            }
        });
        Fake { url, rounds }
    }

    fn rounds(&self) -> usize {
        *self.rounds.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Drain one HTTP request off the socket: headers, then the body `content-length` names.
fn read_request(stream: &std::net::TcpStream) -> String {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let mut len = 0usize;
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head).is_err() || head.trim_end().is_empty() {
            break;
        }
        if let Some(v) = head.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// One round as SSE - the three frames `letibot-provider` parses, in the shapes
/// `crates/bridge/tests/end_to_end.rs` serves, so the two fakes cannot drift on the wire bytes.
fn write_sse(mut stream: std::net::TcpStream, reply: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut body = String::new();
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content": reply}}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
    ));
    body.push_str(&format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{
            "prompt_tokens": 50,
            "completion_tokens": 9,
            "prompt_cache_hit_tokens": 20,
        }})
    ));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
