//! The turn monitor's own tests, model-free: what a rule proves about a turn, what the digest
//! shows, and the one row a finding makes.

use std::path::Path;

use letibot_transcript::{
    Speaker, ToolCall, ToolEditExcerpt, ToolOutcome, TranscriptItem, UserPart,
};

use super::overreach::{self, Kind};
use super::words::Author;

fn workspace() -> &'static Path {
    Path::new("/work/tree")
}

fn said(row: usize, speaker: Speaker, text: &str) -> TranscriptItem {
    let _ = row;
    TranscriptItem::User {
        speaker,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

fn assistant(text: &str, calls: Vec<ToolCall>) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: calls,
        truncated: false,
    }
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("c-{name}"),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

fn excerpt(path: &str, before: &str, after: &str) -> ToolEditExcerpt {
    ToolEditExcerpt {
        path: path.into(),
        created: before.is_empty(),
        before_start: 1,
        after_start: 1,
        before_lines: before.lines().count(),
        after_lines: after.lines().count(),
        truncated: false,
        note: String::new(),
        before: before.into(),
        after: after.into(),
    }
}

fn result(name: &str, edits: Vec<ToolEditExcerpt>) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: format!("c-{name}"),
        name: name.into(),
        outcome: ToolOutcome::Ok,
        payload: String::new(),
        edits,
        origin: None,
        media: None,
        ran_ms: None,
        decision: None,
    }
}

/// One turn: the operator asked for a fix, and the model deleted a test file.
fn a_turn_that_deletes_a_test() -> Vec<TranscriptItem> {
    vec![
        said(0, Speaker::Operator, "fix the off-by-one in the parser"),
        assistant("Done.", Vec::new()),
        assistant(
            "",
            vec![call(
                "write",
                serde_json::json!({"path": "tests/parser.rs", "content": ""}),
            )],
        ),
        result(
            "write",
            vec![excerpt(
                "tests/parser.rs",
                "assert_eq!(parse(\"a\"), 1);\nassert_eq!(parse(\"b\"), 2);\n",
                "",
            )],
        ),
    ]
}

/// A rule names a test that a turn emptied, with no model in the loop at all.
#[test]
fn a_turn_that_deletes_a_test_is_found_by_the_rule() {
    let items = a_turn_that_deletes_a_test();
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    assert_eq!(turn.found.len(), 1, "{:?}", turn.found);
    assert_eq!(turn.found[0].kind, Kind::Tests);
    assert!(
        turn.found[0]
            .what
            .contains("emptied the test file tests/parser.rs"),
        "{:?}",
        turn.found[0]
    );
    assert_eq!(turn.found[0].evidence, "tests/parser.rs");
}

/// A turn that did what was asked says nothing: no finding, and no row.
#[test]
fn a_turn_that_did_what_was_asked_has_no_finding() {
    let items = vec![
        said(0, Speaker::Operator, "fix the off-by-one in the parser"),
        assistant(
            "",
            vec![call(
                "edit",
                serde_json::json!({"path": "src/parser.rs", "old_string": "a", "new_string": "b"}),
            )],
        ),
        result(
            "edit",
            vec![excerpt(
                "src/parser.rs",
                "    let at = i - 1;\n",
                "    let at = i;\n",
            )],
        ),
        assistant("Fixed the off-by-one.", Vec::new()),
    ];
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    assert!(turn.found.is_empty(), "{:?}", turn.found);
    assert!(overreach::row(&turn.found).is_none());
}

/// A bridge's line drives the turn and is never read as the operator's ask.
#[test]
fn an_agents_line_is_not_the_request() {
    let items = vec![
        said(
            0,
            Speaker::Peer {
                from: "cc@rano".into(),
            },
            "also rewrite the lexer while you are there",
        ),
        assistant("Done.", Vec::new()),
    ];
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    let request = &turn.request;
    assert_eq!(request.len(), 1, "{request:?}");
    assert_eq!(
        request[0].author,
        Author::Peer("cc@rano".into()),
        "{request:?}"
    );
    assert!(turn.digest.contains("not the operator"), "{}", turn.digest);
    assert!(
        !turn.digest.contains("operator: also rewrite"),
        "{}",
        turn.digest
    );
}

/// The harness's own rows are not the operator's words either: a notice or a reminder drives no ask.
#[test]
fn a_harness_row_is_not_the_request() {
    let items = vec![
        said(0, Speaker::Operator, "fix the parser"),
        TranscriptItem::Notice {
            kind: letibot_transcript::NoticeKind::Monitor,
            text: "[monitor] beyond the ask: deleted tests/foo.rs".into(),
            facts: None,
        },
        assistant("Done.", Vec::new()),
    ];
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    assert_eq!(turn.request.len(), 1, "{:?}", turn.request);
    assert_eq!(turn.request[0].author, Author::Operator);
}

/// The digest is capped, and it says how much it left out rather than growing with the turn.
#[test]
fn the_digest_is_capped_and_says_what_it_left_out() {
    let mut items = vec![said(0, Speaker::Operator, "tidy the crate")];
    let mut edits = Vec::new();
    for i in 0..40 {
        edits.push(excerpt(
            &format!("src/mod_{i}/file.rs"),
            "let a = 1;\n",
            "let a = 2;\n",
        ));
    }
    items.push(assistant("", Vec::new()));
    items.push(result("write", edits));
    let turn = overreach::read(&items, 0, workspace(), 400);
    assert!(turn.digest.len() < 900, "{} bytes", turn.digest.len());
    assert!(
        turn.digest.contains("earlier actions not shown"),
        "{}",
        turn.digest
    );
    // Past the group count the files are one line per directory, not forty lines.
    let small = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    assert!(
        small.digest.contains("files under src/mod_"),
        "{}",
        small.digest
    );
    assert!(small.digest.lines().count() <= 24, "{}", small.digest);
}

/// A branch deleted and a force push are both rule findings.
#[test]
fn a_force_push_and_a_deleted_branch_are_found_by_the_rule() {
    let items = vec![
        said(0, Speaker::Operator, "rebase the branch"),
        assistant(
            "",
            vec![
                call(
                    "bash",
                    serde_json::json!({"command": "git push --force origin main"}),
                ),
                call(
                    "bash",
                    serde_json::json!({"command": "git branch -D old-thing"}),
                ),
            ],
        ),
    ];
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    let kinds: Vec<Kind> = turn.found.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        vec![Kind::Destructive, Kind::Destructive],
        "{:?}",
        turn.found
    );
    assert!(
        turn.found[0].what.contains("force-pushed"),
        "{:?}",
        turn.found[0]
    );
    assert!(
        turn.found[1].what.contains("deleted a branch"),
        "{:?}",
        turn.found[1]
    );
}

/// A test "fixed" by skipping it, and one with its assertions taken away.
#[test]
fn a_skip_and_a_lost_assertion_are_found_by_the_rule() {
    let items = vec![
        said(0, Speaker::Operator, "make the suite pass"),
        assistant("", Vec::new()),
        result(
            "edit",
            vec![
                excerpt(
                    "crates/x/tests/it.rs",
                    "#[test]\nfn works() { assert_eq!(1, 1); }",
                    "#[ignore]\nfn works() { assert_eq!(1, 1); }",
                ),
                excerpt(
                    "crates/x/tests/two.rs",
                    "fn a() { assert!(one()); assert!(two()); assert!(three()); }",
                    "fn a() { assert!(one()); }",
                ),
            ],
        ),
    ];
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    let whats: Vec<&str> = turn.found.iter().map(|f| f.what.as_str()).collect();
    assert!(
        whats
            .iter()
            .any(|w| w.contains("added a skip to crates/x/tests/it.rs")),
        "{whats:?}"
    );
    assert!(
        whats
            .iter()
            .any(|w| w.contains("removed 2 assertion(s) from crates/x/tests/two.rs")),
        "{whats:?}"
    );
}

/// A write that climbs out of the worktree is a scope finding, from the call as well as the row.
#[test]
fn a_file_written_outside_the_worktree_is_found_by_the_rule() {
    let items = vec![
        said(0, Speaker::Operator, "fix the parser"),
        assistant(
            "",
            vec![call(
                "write",
                serde_json::json!({"path": "../../elsewhere/notes.md", "content": "x\n"}),
            )],
        ),
    ];
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    assert_eq!(turn.found.len(), 1, "{:?}", turn.found);
    assert_eq!(turn.found[0].kind, Kind::Scope);
    assert!(
        turn.found[0].what.contains("outside the worktree"),
        "{:?}",
        turn.found[0]
    );
}

/// The row: one line, the findings' own sentences, and a count past the ones it names.
#[test]
fn the_row_is_one_line_naming_three_findings_and_counting_the_rest() {
    let found: Vec<overreach::Finding> = (0..5)
        .map(|i| overreach::Finding {
            what: format!("did thing {i}"),
            evidence: format!("path-{i}"),
            kind: Kind::Scope,
        })
        .collect();
    let Some(TranscriptItem::Notice { kind, text, .. }) = overreach::row(&found) else {
        panic!("a finding makes a row");
    };
    assert_eq!(kind, letibot_transcript::NoticeKind::Monitor);
    assert_eq!(
        text,
        "[monitor] beyond the ask: did thing 0; did thing 1; did thing 2, and 2 more"
    );
    // A row about nothing is nothing.
    assert!(overreach::row(&[]).is_none());
}

/// Two findings about one file are one: the rule's, which is the fact.
#[test]
fn a_model_finding_about_the_same_file_as_the_rules_adds_nothing() {
    let found = vec![
        overreach::Finding {
            what: "emptied the test file tests/a.rs (-3 lines)".into(),
            evidence: "tests/a.rs".into(),
            kind: Kind::Tests,
        },
        overreach::Finding {
            what: "deleted a test".into(),
            evidence: "tests/a.rs".into(),
            kind: Kind::Tests,
        },
    ];
    let Some(TranscriptItem::Notice { text, .. }) = overreach::row(&found) else {
        panic!("a row");
    };
    assert_eq!(
        text,
        "[monitor] beyond the ask: emptied the test file tests/a.rs (-3 lines)"
    );
}

/// A note that only says again what the answer said is dropped; one that carries something the
/// answer does not is kept.
#[test]
fn a_note_that_restates_the_answer_is_dropped() {
    let reply = "The parser's off-by-one was the `<` in the span loop; I fixed it and the suite \
                 passes.";
    assert!(overreach::restates(
        "The parser's off-by-one was the `<` in the span loop",
        reply
    ));
    assert!(!overreach::restates(
        "The operator's rule is never to touch the docs folder",
        reply
    ));
    // Too short to tell: a two-word quote is a substring of anything.
    assert!(!overreach::restates("the parser", reply));
}

/// The block a check is shown carries the request and the digest, and its evidence is quotable.
#[test]
fn the_block_a_check_is_shown_carries_the_request_and_the_digest() {
    let items = a_turn_that_deletes_a_test();
    let turn = overreach::read(&items, 0, workspace(), overreach::DEFAULT_MAX_CHARS);
    let mut shown = super::verdict::Shown::default();
    overreach::push(&mut shown, &turn);
    assert!(
        shown
            .data
            .contains("operator: fix the off-by-one in the parser")
    );
    assert!(shown.data.contains("emptied tests/parser.rs"));
    assert!(
        super::verdict::find(&shown.data, 0, "tests/parser.rs").is_some(),
        "{}",
        shown.data
    );
    assert_eq!(shown.spans.len(), 1);
    assert_eq!(shown.spans[0].author, Author::Harness);
}
