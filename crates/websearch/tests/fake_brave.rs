//! The whole chain against a stand-in Brave: what goes out on the wire, what
//! comes back as `SearchResults`, and what each failure says.
//!
//! A fake rather than the real API for the reason every other seam in this tree
//! has one: a test that needs a paid key and a network is a test nobody runs, and
//! the thing under test here is our request and our parsing, not Brave's uptime.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use letibot_tools::builtins::external::web::{SearchError, SearchProvider, SearchQuery};
use letibot_websearch::{Brave, KeySource};

struct Fake {
    url: String,
    /// (request line, subscription token) per request.
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

fn fake(body: &'static str, status: u16) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/res/v1/web/search",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut token = String::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    break;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if h.to_ascii_lowercase().starts_with("x-subscription-token:") {
                    token = h[21..].trim().to_string();
                }
            }
            seen2
                .lock()
                .unwrap()
                .push((request_line.trim().to_string(), token));
            let _ = write!(
                conn,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Fake { url, seen }
}

fn brave(f: &Fake) -> Brave {
    Brave::with_key(
        "BSA-test-key".into(),
        KeySource::Env("BRAVE_API_KEY"),
        &f.url,
    )
}

const TWO_HITS: &str = r#"{
  "query": {"original": "rust ownership", "altered": "rust ownership model"},
  "web": {"results": [
    {"title": "Understanding <strong>Ownership</strong>",
     "url": "https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html",
     "description": "<strong>Ownership</strong> is Rust's most unique feature."},
    {"title": "References and Borrowing",
     "url": "https://doc.rust-lang.org/book/ch04-02-references-and-borrowing.html",
     "description": "A reference is like a pointer."},
    {"title": "No URL here", "description": "unreachable"}
  ]},
  "faq": {"results": [{"question": "what is ownership", "answer": "not a web hit"}]}
}"#;

#[test]
fn the_request_carries_the_key_and_the_answer_is_parsed_without_markup() {
    let f = fake(TWO_HITS, 200);
    let p = brave(&f);
    let out = p
        .search(&SearchQuery {
            query: "rust ownership".into(),
            max_results: 2,
            site: None,
        })
        .expect("a 200 with hits");

    // The wire: the key in Brave's own header, the query and the count.
    let seen = f.seen.lock().unwrap();
    let (line, token) = seen.first().expect("one request").clone();
    assert_eq!(
        token, "BSA-test-key",
        "the key must ride in X-Subscription-Token"
    );
    assert!(
        line.contains("q=rust%20ownership") || line.contains("q=rust+ownership"),
        "{line}"
    );
    assert!(line.contains("count="), "{line}");

    // The answer: markup stripped, the URL-less hit dropped, the cap applied, and
    // the denominator kept — `considered` counts what Brave returned, which is
    // what makes "showing 2" different from "there were only 2".
    assert_eq!(out.hits.len(), 2);
    assert_eq!(out.hits[0].title, "Understanding Ownership");
    assert_eq!(
        out.hits[0].snippet,
        "Ownership is Rust's most unique feature."
    );
    assert!(out.hits[0].url.starts_with("https://doc.rust-lang.org/"));
    assert_eq!(
        out.considered, 3,
        "the denominator is what the provider had"
    );
    assert_eq!(out.provider, "Brave Search");
    assert_eq!(out.rewritten_query.as_deref(), Some("rust ownership model"));
    // The FAQ block is not a web hit and must never be reported as one.
    assert!(
        out.hits
            .iter()
            .all(|h| !h.snippet.contains("not a web hit"))
    );
}

#[test]
fn a_site_restriction_goes_out_as_braves_own_operator() {
    let f = fake(TWO_HITS, 200);
    let p = brave(&f);
    p.search(&SearchQuery {
        query: "ownership".into(),
        max_results: 5,
        site: Some("doc.rust-lang.org".into()),
    })
    .expect("ok");
    let seen = f.seen.lock().unwrap();
    let (line, _) = seen.first().unwrap().clone();
    assert!(
        line.contains("site%3Adoc.rust-lang.org") || line.contains("site:doc.rust-lang.org"),
        "{line}"
    );
}

#[test]
fn each_failure_says_which_one_it_is() {
    // A bad key is a refusal that names where the key came from, so the operator
    // knows which of the three places to fix.
    let f = fake(r#"{"error":"bad key"}"#, 401);
    let e = brave(&f)
        .search(&SearchQuery {
            query: "x".into(),
            max_results: 1,
            site: None,
        })
        .expect_err("401");
    match e {
        SearchError::Refused(m) => {
            assert!(m.contains("401"), "{m}");
            assert!(
                m.contains("BRAVE_API_KEY"),
                "the refusal must name the key's source: {m}"
            );
        }
        other => panic!("a 401 is a refusal, not {other:?}"),
    }

    // A rate limit is the one every free-tier key hits, so it gets its own words.
    let f = fake(r#"{}"#, 429);
    let e = brave(&f)
        .search(&SearchQuery {
            query: "x".into(),
            max_results: 1,
            site: None,
        })
        .expect_err("429");
    assert!(
        matches!(&e, SearchError::Refused(m) if m.contains("rate-limited")),
        "{e:?}"
    );

    // Nothing listening is transport, not a claim about the world.
    let p = Brave::with_key(
        "k".into(),
        KeySource::Flag,
        "http://127.0.0.1:1/res/v1/web/search",
    );
    let e = p
        .search(&SearchQuery {
            query: "x".into(),
            max_results: 1,
            site: None,
        })
        .expect_err("no server");
    assert!(matches!(e, SearchError::Transport(_)), "{e:?}");
}

#[test]
fn a_query_that_matched_nothing_is_empty_rather_than_invented() {
    let f = fake(r#"{"query":{"original":"zzz"},"web":{"results":[]}}"#, 200);
    let out = brave(&f)
        .search(&SearchQuery {
            query: "zzz".into(),
            max_results: 5,
            site: None,
        })
        .expect("200");
    assert!(out.hits.is_empty());
    assert_eq!(out.considered, 0);
    assert_eq!(out.rewritten_query, None);
}
