//! **Claude Code's store, read the way this layer reads it — tested where the reading lives.**
//!
//! These run natively (`cargo test` in this directory): the crate is a `cdylib` for the host and an
//! `rlib` for these, and nothing here knows that a wasm ABI exists. What they cover is the half the
//! host cannot check for it — how one of Claude Code's own lines becomes an ask or a conclusion,
//! how a path becomes a project directory's name, and that every answer says what it left out.

use super::*;

/// One call, exactly as the host makes it.
fn call(args: Value, looks: Vec<Value>, state: Value) -> Value {
    let input = json!({
        "v": 1,
        "name": "claude_transcript",
        "args": args,
        "feeds": {
            "home": "/home/dead",
            "workspace": "/home/dead/Projects/letibot",
            "session": "s-1",
            "now_ms": 0,
        },
        "looks": looks,
        "state": state,
    });
    let out = answer(input.to_string().as_bytes()).expect("the layer always answers");
    serde_json::from_str(&out).expect("an answer is JSON")
}

/// Drive one call through its rounds: `answers` is what the host hands back each round.
fn drive(args: Value, answers: Vec<Vec<Value>>) -> (Vec<Value>, Value) {
    let mut looks: Vec<Value> = Vec::new();
    let mut state = Value::Null;
    let mut asked: Vec<Value> = Vec::new();
    let mut round = 0usize;
    loop {
        let out = call(args.clone(), looks, state);
        let Some(wanted) = out.get("looks").and_then(Value::as_array) else {
            return (asked, out);
        };
        asked.extend(wanted.iter().cloned());
        state = out.get("state").cloned().unwrap_or(Value::Null);
        let Some(next) = answers.get(round) else {
            panic!("the layer kept asking for looks: {asked:?}");
        };
        looks = next.clone();
        round += 1;
    }
}

fn payload(answer: &Value) -> String {
    answer
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One transcript line of Claude Code's own shape.
fn user_line(text: &str) -> String {
    json!({
        "type": "user",
        "isSidechain": false,
        "userType": "external",
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

fn assistant_line(text: &str) -> String {
    json!({
        "type": "assistant",
        "isSidechain": false,
        "message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "not this"},
            {"type": "text", "text": text},
            {"type": "tool_use", "name": "Bash", "input": {}},
        ]},
    })
    .to_string()
}

fn entry(name: &str, dir: bool, size: u64, mtime: u64) -> Value {
    json!({"name": name, "dir": dir, "size": size, "mtime": mtime})
}

fn listed(entries: Vec<Value>) -> Value {
    json!({"exit": 0, "entries": entries, "more": false})
}

fn lines(values: Vec<(u64, String)>) -> Value {
    json!({
        "exit": 0,
        "lines": values.into_iter().map(|(n, text)| json!({
            "n": n, "text": text, "cut": false
        })).collect::<Vec<Value>>(),
        "more": false,
    })
}

// ---------------------------------------------------------------------------------------------
// One line of the store
// ---------------------------------------------------------------------------------------------

#[test]
fn an_operators_line_is_an_ask_and_a_models_text_is_a_conclusion() {
    match row_of(&user_line("lets build it for mac")) {
        Some(Row::Ask { text }) => assert_eq!(text, "lets build it for mac"),
        other => panic!("a user row is an ask: {other:?}", other = other.is_some()),
    }
    match row_of(&assistant_line("the build is green")) {
        // The thinking block and the tool call are NOT in it: an outline is what was concluded,
        // and a model's reasoning is not something the operator ever read.
        Some(Row::Conclusion { text }) => assert_eq!(text, "the build is green"),
        other => panic!("an assistant row is a conclusion: {}", other.is_some()),
    }
}

#[test]
fn tool_traffic_and_sidechains_are_not_conversation() {
    // A tool result handed back to the model: a `user` row whose content is a list of results.
    let tool_result = json!({
        "type": "user",
        "isSidechain": false,
        "userType": "external",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "content": "a wall of output"},
        ]},
    })
    .to_string();
    assert!(
        row_of(&tool_result).is_none(),
        "a tool result is not an ask"
    );

    // The harness talking to itself, which Claude Code records as an `internal` user row.
    let internal = json!({
        "type": "user",
        "userType": "internal",
        "message": {"role": "user", "content": "<command-name>/clear</command-name>"},
    })
    .to_string();
    assert!(
        row_of(&internal).is_none(),
        "an internal row is not the operator"
    );

    // A subagent's conversation, replayed into the same file.
    let sidechain = json!({
        "type": "user",
        "isSidechain": true,
        "userType": "external",
        "message": {"role": "user", "content": "the child's brief"},
    })
    .to_string();
    assert!(
        row_of(&sidechain).is_none(),
        "a sidechain is another conversation"
    );

    // And the rows that are not conversation at all.
    assert!(row_of(r#"{"type":"ai-title","aiTitle":"x"}"#).is_none());
    assert!(row_of("not json").is_none());
    // A line the host cut cannot be parsed, and half a sentence is worse than the next row.
    assert!(
        row_of("{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"lets bu").is_none()
    );
}

#[test]
fn the_first_operator_line_skips_the_header_rows() {
    let head = lines(vec![
        (1, r#"{"type":"ai-title","aiTitle":"a title"}"#.to_string()),
        (2, r#"{"type":"mode","mode":"normal"}"#.to_string()),
        (3, user_line("ok, i need your help setting up this mac")),
        (4, user_line("and then the theme")),
    ]);
    assert_eq!(
        first_operator_line(&head).as_deref(),
        Some("ok, i need your help setting up this mac")
    );
}

// ---------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------

#[test]
fn a_project_path_becomes_the_directory_claude_code_gave_it() {
    assert_eq!(
        slug("/Users/dead/Projects/letibot", "/Users/dead"),
        "-Users-dead-Projects-letibot"
    );
    // A dot is a separator too — this box has `-Users-dead--emacs-d` for `~/.emacs.d`.
    assert_eq!(
        slug("/Users/dead/.emacs.d", "/Users/dead"),
        "-Users-dead--emacs-d"
    );
    // A hyphen in a name survives, because it is one of the characters that is kept.
    assert_eq!(
        slug("/Users/dead/Projects/stroppy-io", "/Users/dead"),
        "-Users-dead-Projects-stroppy-io"
    );
    // `~/` expands against the home the HOST handed over, not this process's.
    assert_eq!(slug("~/x", "/home/dead"), "-home-dead-x");
}

#[test]
fn the_root_is_homes_claude_directory() {
    let feeds = Feeds {
        home: "/home/dead".into(),
        ..Feeds::default()
    };
    assert_eq!(root(&feeds).unwrap(), "/home/dead/.claude/projects");
    let none = Feeds::default();
    let why = root(&none).unwrap_err();
    assert!(why.0.contains("home"), "{}", why.0);
}

// ---------------------------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------------------------

#[test]
fn sessions_lists_projects_then_transcripts_then_their_first_lines() {
    let args = json!({"what": "sessions", "all_projects": true});
    let (asked, out) = drive(
        args,
        vec![
            // The root, then one listing per project.
            vec![listed(vec![entry(
                "-home-dead-Projects-letibot",
                true,
                0,
                10,
            )])],
            vec![listed(vec![
                entry("a.jsonl", false, 2048, 900),
                entry("b.jsonl", false, 4096, 500),
                entry("tool-results", true, 0, 500),
            ])],
            // One head per transcript, newest first.
            vec![
                lines(vec![(1, user_line("build it for the mac"))]),
                lines(vec![(1, user_line("what is in the queue"))]),
            ],
        ],
    );
    // The first thing asked for is the root itself, and it is the host that resolves `~`.
    assert_eq!(asked[0]["list"], json!("/home/dead/.claude/projects"));
    assert_eq!(out["outcome"], json!("ok"), "{}", payload(&out));
    let said = payload(&out);
    assert!(said.contains("a.jsonl") || said.contains("/a "), "{said}");
    assert!(said.contains("build it for the mac"), "{said}");
    assert!(said.contains("what is in the queue"), "{said}");
    // The directory beside the transcripts is not a transcript.
    assert!(!said.contains("tool-results"), "{said}");
}

#[test]
fn sessions_with_a_project_named_lists_it_directly() {
    let args = json!({"what": "sessions", "project": "/Users/dead/Projects/letibot"});
    let (asked, out) = drive(
        args,
        vec![
            vec![listed(vec![entry("a.jsonl", false, 100, 900)])],
            vec![lines(vec![(1, user_line("the first thing"))])],
        ],
    );
    assert_eq!(
        asked[0]["list"],
        json!("/home/dead/.claude/projects/-Users-dead-Projects-letibot")
    );
    assert_eq!(out["outcome"], json!("ok"), "{}", payload(&out));
    assert!(
        payload(&out).contains("the first thing"),
        "{}",
        payload(&out)
    );
}

#[test]
fn an_empty_project_says_so_rather_than_listing_nothing() {
    let args = json!({"what": "sessions", "project": "/Users/dead/Projects/letibot"});
    let (_asked, out) = drive(args, vec![vec![listed(vec![])]]);
    assert_eq!(out["outcome"], json!("ok"));
    assert!(
        payload(&out).contains("no transcripts"),
        "{}",
        payload(&out)
    );
}

#[test]
fn search_without_a_needle_is_refused_and_says_what_it_needs() {
    let (_asked, out) = drive(json!({"what": "search"}), vec![]);
    assert_eq!(out["outcome"], json!("refused"));
    assert!(payload(&out).contains("`match`"), "{}", payload(&out));
}

#[test]
fn search_answers_line_numbers_byte_offsets_and_context() {
    let args = json!({"what": "search", "match": "queue", "limit": 5, "context": 1});
    let (asked, out) = drive(
        args,
        vec![
            vec![listed(vec![entry("a.jsonl", false, 100, 900)])],
            vec![json!({
                "exit": 0,
                "matches": [{
                    "line": 42,
                    "byte": 4096,
                    "text": "{\"type\":\"user\",\"message\":{\"content\":\"the queue is red\"}}",
                    "cut": false,
                    "before": [{"n": 41, "text": "a line before", "cut": false}],
                    "after": [{"n": 43, "text": "a line after", "cut": false}],
                }],
                "scanned": 900,
                "more": false,
            })],
        ],
    );
    assert_eq!(
        asked[1]["grep"],
        json!("/home/dead/.claude/projects/-home-dead-Projects-letibot/a.jsonl")
    );
    assert_eq!(asked[1]["match"], json!("queue"));
    let said = payload(&out);
    assert!(said.contains(":42"), "{said}");
    assert!(said.contains("byte 4096"), "{said}");
    assert!(said.contains("a line before"), "{said}");
    assert!(said.contains("a line after"), "{said}");
    // The count is of what was READ, so a miss and a failure cannot be confused.
    assert!(said.contains("900 line(s)"), "{said}");
}

#[test]
fn a_search_that_matches_nothing_is_an_empty_result_not_a_failure() {
    let args = json!({"what": "search", "match": "nothing at all"});
    let (_asked, out) = drive(
        args,
        vec![
            vec![listed(vec![entry("a.jsonl", false, 100, 900)])],
            vec![json!({"exit": 0, "matches": [], "scanned": 57, "more": false})],
        ],
    );
    assert_eq!(out["outcome"], json!("ok"));
    let said = payload(&out);
    assert!(said.contains("nothing matched"), "{said}");
    assert!(said.contains("57 line(s)"), "{said}");
    assert!(said.contains("not a regex"), "{said}");
}

#[test]
fn a_transcript_the_host_refused_is_named_rather_than_silently_skipped() {
    let args = json!({"what": "search", "match": "x"});
    let (_asked, out) = drive(
        args,
        vec![
            vec![listed(vec![entry("a.jsonl", false, 100, 900)])],
            vec![json!({"exit": 1, "error": "`/x` is outside every tree this layer may read"})],
        ],
    );
    assert_eq!(out["outcome"], json!("ok"));
    assert!(
        payload(&out).contains("could not be read"),
        "{}",
        payload(&out)
    );
    assert!(
        payload(&out).contains("outside every tree"),
        "{}",
        payload(&out)
    );
}

#[test]
fn read_answers_a_numbered_range() {
    let args = json!({"what": "read", "session": "e90320f4-b550-4aa0-aa11-1c2673df88e8", "from": 40, "lines": 2});
    let (asked, out) = drive(
        args,
        vec![vec![lines(vec![
            (40, user_line("line forty")),
            (41, user_line("line forty one")),
        ])]],
    );
    // A full session id needs no listing first: the file's name is the id.
    assert_eq!(
        asked[0]["read"],
        json!(
            "/home/dead/.claude/projects/-home-dead-Projects-letibot/e90320f4-b550-4aa0-aa11-1c2673df88e8.jsonl"
        )
    );
    let said = payload(&out);
    assert!(said.contains("    40 |"), "{said}");
    assert!(said.contains("line forty one"), "{said}");
}

#[test]
fn a_prefix_resolves_against_the_listing_before_anything_is_read() {
    let args = json!({"what": "read", "session": "e903"});
    let (asked, out) = drive(
        args,
        vec![
            vec![listed(vec![
                entry(
                    "e90320f4-b550-4aa0-aa11-1c2673df88e8.jsonl",
                    false,
                    100,
                    900,
                ),
                entry("253d6c3e-5492-4e10-93d9-d756fae5431e.jsonl", false, 50, 800),
            ])],
            vec![lines(vec![(1, user_line("the first thing"))])],
        ],
    );
    // A prefix is resolved against a listing, so the first look is the directory and the second
    // is the file it named.
    assert_eq!(
        asked[0]["list"],
        json!("/home/dead/.claude/projects/-home-dead-Projects-letibot")
    );
    assert_eq!(
        asked[1]["read"],
        json!(
            "/home/dead/.claude/projects/-home-dead-Projects-letibot/e90320f4-b550-4aa0-aa11-1c2673df88e8.jsonl"
        )
    );
    assert!(
        payload(&out).contains("the first thing"),
        "{}",
        payload(&out)
    );
}

#[test]
fn a_prefix_that_names_two_transcripts_is_refused_with_the_list() {
    // Both files start with `e9`, so the prefix names neither one.
    let args = json!({"what": "read", "session": "e9"});
    let (asked, out) = drive(
        args,
        vec![vec![listed(vec![
            entry(
                "e90320f4-b550-4aa0-aa11-1c2673df88e8.jsonl",
                false,
                100,
                900,
            ),
            entry("e9c0d8e1-a517-4288-8375-0e82f436c3cf.jsonl", false, 50, 800),
        ])]],
    );
    assert_eq!(asked.len(), 1, "it asked for the listing and then answered");
    assert_eq!(out["outcome"], json!("failed"));
    let said = payload(&out);
    assert!(said.contains("names 2 transcripts"), "{said}");
    assert!(said.contains("e90320f4"), "{said}");
    assert!(said.contains("e9c0d8e1"), "{said}");
}

#[test]
fn outline_reads_the_two_row_kinds_and_answers_without_tool_traffic() {
    let args = json!({"what": "outline", "session": "e90320f4-b550-4aa0-aa11-1c2673df88e8"});
    let (asked, out) = drive(
        args,
        vec![
            // The two greps, answered together.
            vec![
                json!({"exit": 0, "matches": [{"line": 3, "byte": 0, "text": "u", "cut": false}], "scanned": 10, "more": false}),
                json!({"exit": 0, "matches": [{"line": 9, "byte": 0, "text": "a", "cut": false}], "scanned": 10, "more": false}),
            ],
            // The lines those named, read in one look.
            vec![lines(vec![
                (3, user_line("build it for the mac")),
                (9, assistant_line("the build is green")),
            ])],
        ],
    );
    assert_eq!(asked[0]["grep"], asked[1]["grep"], "one file, two kinds");
    assert_eq!(asked[2]["read"], asked[0]["grep"]);
    assert_eq!(asked[2]["at"], json!([3, 9]));
    let said = payload(&out);
    assert!(said.contains("OPERATOR"), "{said}");
    assert!(said.contains("build it for the mac"), "{said}");
    assert!(said.contains("the build is green"), "{said}");
    assert!(said.contains("left out on purpose"), "{said}");
}

#[test]
fn an_unknown_operation_is_a_refusal_that_lists_the_four() {
    let (_asked, out) = drive(json!({"what": "sql"}), vec![]);
    assert_eq!(out["outcome"], json!("failed"));
    let said = payload(&out);
    for op in ["sessions", "search", "read", "outline"] {
        assert!(said.contains(op), "`{op}` should be named in `{said}`");
    }
}

// ---------------------------------------------------------------------------------------------
// The small words
// ---------------------------------------------------------------------------------------------

#[test]
fn a_clip_says_how_much_it_left() {
    let short = clip("a short line", 40);
    assert_eq!(short, "a short line");
    let long = clip(&"x".repeat(100), 10);
    assert!(long.starts_with(&"x".repeat(10)), "{long}");
    assert!(long.contains("90 more characters"), "{long}");
}

#[test]
fn a_stamp_is_a_date_and_how_long_ago() {
    // 2026-10-28T22:38:00Z, which is the mtime of the transcript this tool was written against.
    let ms = 1_793_227_080_000u64;
    let said = stamp(ms, ms + 3 * 3_600_000);
    assert_eq!(said, "2026-10-28 22:38 (3 h ago)", "{said}");
    // A caller with no clock gets the date alone rather than a wrong age.
    assert_eq!(stamp(ms, 0), "2026-10-28 22:38");
}

#[test]
fn a_size_is_read_at_a_glance() {
    assert_eq!(size(12), "12 B");
    assert_eq!(size(2048), "2.0 kB");
    assert_eq!(size(116_826_114), "111.4 MB");
}
