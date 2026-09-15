//! The real Brave API. `BRAVE_LIVE=1 cargo test -p letibot-websearch --test brave_live`.
//!
//! The one thing the stand-in in `fake_brave.rs` cannot check: whether Brave's
//! actual JSON field names are the ones this crate reads. Those were written
//! from the documented shape, so until this runs, `web.results[].description`
//! and friends are an assumption.
//!
//! Costs one query against the operator's key (free tier: 1/second), so it is
//! one search and it asserts on structure, never on what the web happens to say
//! today.

use letibot_tools::builtins::external::web::{SearchProvider, SearchQuery};
use letibot_websearch::Brave;

#[test]
fn brave_answers_with_the_fields_this_crate_reads() {
    if std::env::var("BRAVE_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: BRAVE_LIVE is not 1 — the check did not run and this is not a pass");
        return;
    }
    let brave = match Brave::attach(None) {
        Ok(b) => b,
        Err(why) => panic!("no key: {why}"),
    };
    eprintln!("attached: {}", brave.describe());

    let out = brave
        .search(&SearchQuery {
            query: "rust programming language".into(),
            max_results: 3,
            site: None,
        })
        .expect("a live search");

    // Structure, not content. If the field names were wrong every one of these
    // would be empty while the request itself succeeded — which is exactly the
    // silent-wrong-answer shape this test exists to catch.
    assert!(!out.hits.is_empty(), "no hits parsed: field names are wrong, or the key has no quota");
    assert!(out.considered >= out.hits.len(), "considered {} < hits {}", out.considered, out.hits.len());
    assert_eq!(out.provider, "Brave Search");
    for h in &out.hits {
        assert!(h.url.starts_with("http"), "url not parsed: {h:?}");
        assert!(!h.title.is_empty(), "title not parsed: {h:?}");
        assert!(!h.title.contains("<strong>"), "markup reached the model: {h:?}");
        assert!(!h.snippet.contains("<strong>"), "markup reached the model: {h:?}");
    }
    eprintln!(
        "{} hit(s) of {} considered; first: {} <{}>",
        out.hits.len(),
        out.considered,
        out.hits[0].title,
        out.hits[0].url
    );
}
