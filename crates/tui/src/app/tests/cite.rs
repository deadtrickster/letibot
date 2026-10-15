//! Citations: a reply's empty `path:start-end` fence, filled with the lines the daemon read.

use super::*;
use letibot_sessionlog::Citation;

/// **The model names a range and the head shows the file's lines with the file's numbers** — the
/// operator: *"model wants to cite me a file with line range … and it is manually extracting the
/// code and line numbers lost"*. Before the daemon's event the fence is empty; after it, the
/// lines are there numbered from 242, and a click on line 244 is a reference to line 244.
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
