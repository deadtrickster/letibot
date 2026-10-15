//! **A reply's citations, read from the file through the session's backend and kept.**

use super::*;

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn published(h: &Harness) -> Vec<Vec<letibot_sessionlog::Citation>> {
    h.hub
        .retained()
        .into_iter()
        .filter_map(|e| match e.event {
            letibot_sessionlog::SessionEvent::Citations { citations, .. } => Some(citations),
            _ => None,
        })
        .collect()
}

/// **The lines are read when the reply lands and the snapshot is what comes back** — a reply
/// published again (a restore, a fork's carry) shows what it cited when it was written, not the
/// file as it is now; a reply that cites nothing publishes nothing.
#[test]
fn a_citation_is_read_once_and_kept_for_the_reply() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = Scratch(letibot_sessionlog::testing::scratch_root().join(format!(
        "cite-{}-{}",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    )));
    std::fs::create_dir_all(dir.0.join("flow")).unwrap();
    std::fs::write(
        dir.0.join("flow/a.go"),
        "package a\n\nfunc A() {}\nfunc B() {}\n",
    )
    .unwrap();
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = "s-cite".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new("s-cite")).expect("the session opens");

    let reply = TranscriptItem::Assistant {
        text: "Here:\n\n```flow/a.go:3-4\n```\n".into(),
        tool_calls: Vec::new(),
        truncated: false,
    };
    h.publish_citations("t.1", &reply);
    let first = published(&h);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0][0].lines, vec!["func A() {}", "func B() {}"]);
    assert_eq!(first[0][0].error, None);

    std::fs::write(dir.0.join("flow/a.go"), "package a\n\nfunc A2() {}\n").unwrap();
    h.publish_citations("t2.1", &reply);
    let again = published(&h);
    assert_eq!(again.len(), 2);
    assert_eq!(
        again[1][0].lines,
        vec!["func A() {}", "func B() {}"],
        "the carried reply shows what it cited when it was written"
    );

    h.publish_citations(
        "t.2",
        &TranscriptItem::Assistant {
            text: "No citations here.\n\n```go\nfunc x() {}\n```\n".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    assert_eq!(
        published(&h).len(),
        2,
        "a reply citing nothing publishes nothing"
    );
}

/// **A pull request the session opened is watched without being asked** (`pr_watch`): the five
/// watches are armed as the session's monitors, and the sentence names them. `gh` is a stub that
/// never answers, so nothing reaches GitHub; an arming that runs twice arms nothing new.
#[test]
fn an_opened_pull_request_gets_its_watches() {
    let Some(gguf) = crate::vocab_gguf() else {
        return;
    };
    let dir = Scratch(letibot_sessionlog::testing::scratch_root().join(format!(
        "prw-{}-{}",
        std::process::id(),
        letibot_sessionlog::registry::now_ms()
    )));
    std::fs::create_dir_all(&dir.0).unwrap();
    let gh = dir.0.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nsleep 5\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // SAFETY: read only by a GitHub condition's constructor, below, in this test.
    unsafe { std::env::set_var("LETIBOT_GH", &gh) };
    let mut cfg = Config::for_this_box(&dir.0);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.seat = crate::config::Seat::Leticode;
    // The operator's own sessions run with `--bash`, which is what seats `monitor`.
    cfg.allow_bash = true;
    cfg.store = Some(dir.0.join("sessions.db"));
    cfg.session_id = "s-prw".into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new("s-prw")).expect("the session opens");
    let said = h.watch_opened_pr("https://github.com/o/r/pull/7", 7);
    assert!(said.contains("is now watched for you"), "{said}");
    let live: Vec<String> = h
        .monitors()
        .expect("a session with a process host has monitors")
        .live_names();
    for want in [
        "pr-7-checks",
        "pr-7-review",
        "pr-7-coderabbit",
        "pr-7-comment",
        "pr-7-closed",
    ] {
        assert!(live.iter().any(|n| n == want), "{want} armed: {live:?}");
    }
    let again = h.watch_opened_pr("https://github.com/o/r/pull/7", 7);
    assert!(
        !again.contains("is now watched"),
        "nothing new to arm: {again}"
    );
    unsafe { std::env::remove_var("LETIBOT_GH") };
}
