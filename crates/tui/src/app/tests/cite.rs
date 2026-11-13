//! Citations: a reply's empty `path:start-end` fence, filled with the lines the daemon read.

use super::*;
use letibot_sessionlog::Citation;

/// The model names a range and the head shows the file's lines with the file's numbers: before the
/// daemon's event the fence is empty, after it the lines are there numbered from 242, and a click
/// on line 244 is a reference to line 244.
#[test]
fn a_cited_range_shows_the_files_lines_with_its_numbers_and_a_line_clicks_to_its_place() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.1", "assistant"),
    )));
    a.record_item(
        "r.1",
        TranscriptItem::Assistant {
            text: "The schema dump:\n\n```flow/pgdump_schema.go:242-244\n```\n\nThat is all."
                .into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    let before = a.screen(100, 30).join("\n");
    assert!(
        before.contains("┌─ flow/pgdump_schema.go:242-244"),
        "{before}"
    );
    assert!(!before.contains("242  "), "nothing cited yet: {before}");
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Citations {
            item_id: "r.1".into(),
            citations: vec![Citation {
                label: "flow/pgdump_schema.go:242-244".into(),
                path: "flow/pgdump_schema.go".into(),
                start: 242,
                end: 244,
                lines: vec![
                    "func dump() error {".into(),
                    "\treturn nil".into(),
                    "}".into(),
                ],
                error: None,
                kind: letibot_sessionlog::CitationKind::Cite,
                body: String::new(),
            }],
        },
    )));
    let screen = a.screen(100, 30);
    let text = screen.join("\n");
    assert!(text.contains("│ 242  func dump() error {"), "{text}");
    assert!(text.contains("│ 244  }"), "{text}");
    // Each drawn line of the citation is a reference to its own line; the prose is none.
    let mut named: Vec<usize> = a
        .file_rows
        .iter()
        .filter(|(_, f)| f.path == "flow/pgdump_schema.go" && f.change.is_none())
        .map(|(_, f)| f.line)
        .collect();
    named.dedup();
    assert_eq!(
        named,
        vec![242, 243, 244],
        "the header and each line, at its own number"
    );
    let _ = screen;
}

/// The same range is a mark on the file, kept by the head for a file that is not open: rano holds a
/// mark only while its buffer is, so the pane is handed this when the file opens.
#[test]
fn a_cited_range_is_a_cite_mark_on_those_lines() {
    use rano::marks::MarkKind;
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.9", "assistant"),
    )));
    a.record_item(
        "r.9",
        TranscriptItem::Assistant {
            text: "```flow/pgdump_schema.go:242-244\n```".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Citations {
            item_id: "r.9".into(),
            citations: vec![Citation {
                label: "flow/pgdump_schema.go:242-244".into(),
                path: "flow/pgdump_schema.go".into(),
                start: 242,
                end: 244,
                lines: vec![
                    "func dump() error {".into(),
                    "\treturn nil".into(),
                    "}".into(),
                ],
                error: None,
            }],
        },
    )));
    let groups = a
        .file_marks
        .groups(std::path::Path::new("flow/pgdump_schema.go"));
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].0, "reply:r.9", "owned by the reply that made it");
    let mark = &groups[0].1[0];
    assert_eq!(mark.kind, MarkKind::Cite);
    assert_eq!((mark.line, mark.end_line), (242, 244));
    assert_eq!(
        mark.anchor,
        vec!["func dump() error {", "\treturn nil", "}"],
        "the lines as the file had them: what says later whether it still fits"
    );
}

/// A citation whose range could not be read is no mark: there is no anchor to land on, and the row
/// already carries the sentence saying why it is empty.
#[test]
fn an_unreadable_range_is_not_a_mark() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("r.8", "assistant"),
    )));
    a.record_item(
        "r.8",
        TranscriptItem::Assistant {
            text: "```gone.txt:9-10\n```".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Citations {
            item_id: "r.8".into(),
            citations: vec![Citation {
                label: "gone.txt:9-10".into(),
                path: "gone.txt".into(),
                start: 9,
                end: 10,
                lines: Vec::new(),
                error: Some("no such file".into()),
            }],
        },
    )));
    assert!(
        a.file_marks
            .groups(std::path::Path::new("gone.txt"))
            .is_empty()
    );
}
