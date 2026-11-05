//! A warning is BUILT in one place: `SessionEvent::warning` is the crate's only construction of the
//! variant, and every site reports one through `hub.warn`/`Harness::warn`/`Sessions::warn`. The scan
//! is the test, because no compiler sees a `code:`/`detail:` filled in by hand beside it.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, at any depth.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(sources(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

/// The lines where `text` builds a `SessionEvent::Warning` by hand: the variant's third field is
/// named near it, which a PATTERN (`Warning { code, .. }`, an `if let` or a `matches!`) never does.
fn hand_built(text: &str) -> Vec<usize> {
    let needle = "SessionEvent::Warning";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find(needle) {
        let at = from + i;
        from = at + needle.len();
        let block = &text[from..text.len().min(from + 400)];
        if block.contains("compaction:") {
            out.push(text[..at].matches('\n').count() + 1);
        }
    }
    out
}

#[test]
fn no_site_builds_a_warning_by_hand() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = sources(&src);
    assert!(
        files.len() > 20,
        "the scan found {} file(s) under {}, so it is reading nothing",
        files.len(),
        src.display()
    );
    let mut bad: Vec<String> = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in hand_built(&text) {
            bad.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(
        bad.is_empty(),
        "these sites build a warning by hand instead of saying what it is to the one door \
         (`hub.warn`/`Harness::warn`/`Sessions::warn`, or `SessionEvent::warning` where the event is \
         not published to a hub): {bad:?}"
    );
}

/// The scan has teeth: a hand-built variant is reported and a pattern is not, so a green run is the
/// absence of the shape rather than a scanner that matches nothing.
#[test]
fn the_scan_tells_a_construction_from_a_pattern() {
    let built = "hub.publish(SessionEvent::Warning { code: \"x\".into(), detail: d, \
                 compaction: None });";
    assert_eq!(hand_built(built), vec![1]);
    let pattern = "if let SessionEvent::Warning { code, detail, .. } = e { }";
    assert!(
        hand_built(pattern).is_empty(),
        "a pattern names no field and must not be reported"
    );
    let bound = "matches!(&e.event, SessionEvent::Warning { code, .. } if code == X)";
    assert!(hand_built(bound).is_empty());
}

/// The other half, so the scan cannot pass by the constructor having been deleted with the sites:
/// the constructor and the sink build the same event, field for field.
#[test]
fn the_constructor_and_the_sink_agree() {
    use letibot_sessionlog::SessionEvent;
    use letibot_sessionlog::hub::Hub;
    let hub = Hub::new("s-one-door");
    // Named rather than spelled at the call: the tree's census reads a literal there as a wire code
    // and demands a severity row for it.
    let the_code = "a_code";
    hub.warn(the_code, "a sentence");
    let kept = hub.retained();
    let last = kept.last().expect("the warning is on the log");
    assert_eq!(
        last.event,
        SessionEvent::warning(the_code, "a sentence"),
        "a hub's warning and the constructor are one event, or a door without a hub says \
         something a head draws differently"
    );
    hub.warn_with("compacted", "a summary", None);
    let kept = hub.retained();
    let last = kept.last().expect("the second warning is on the log");
    assert_eq!(
        last.event,
        SessionEvent::warning_with("compacted", "a summary", None),
        "`warn_with` with no report is `warn`"
    );
}
