//! The headless reviewer's own words: the schema it is asked for, the argv it is started with, and
//! what its stdout says. No process is started here - `claude_tests` runs the real `serve` against a
//! stub binary.

use super::*;

fn settings() -> Settings {
    Settings::new(PathBuf::from("/usr/local/bin/claude"))
}

/// The schema is the queue's OWN verdict, field for field: the words [`Decision::ALL`] holds and
/// the three evidence fields `review_verdict` takes. A second spelling of the answer would be a
/// second answer, and a Claude verdict could then not be written onto a row a letibot reviewer
/// judged with the same code.
#[test]
fn the_schema_is_the_queues_own_verdict_and_nothing_else() {
    let s = schema();
    let words: Vec<&str> = s["properties"]["decision"]["enum"]
        .as_array()
        .expect("the decision's words")
        .iter()
        .map(|v| v.as_str().expect("a word"))
        .collect();
    let want: Vec<&str> = Decision::ALL.iter().map(|d| d.as_str()).collect();
    assert_eq!(words, want, "the enum is `Decision::ALL`, in its order");
    let mut required: Vec<&str> = s["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|v| v.as_str().expect("a field"))
        .collect();
    required.sort_unstable();
    assert_eq!(required, vec!["commands", "decision", "files", "reasons"]);
    assert_eq!(
        s["additionalProperties"], false,
        "a field the queue does not read must not be asked for"
    );
    for field in ["reasons", "files", "commands"] {
        assert_eq!(
            s["properties"][field]["type"], "array",
            "`{field}` is a list of strings"
        );
    }
}

/// The argv a run is started with: the prompt, the output format the queue can read, the schema, the
/// permission mode and the tools - the shape the headless docs give. `--allowedTools` takes every
/// argument until the next flag, so the tools come last.
#[test]
fn the_argv_names_the_prompt_the_schema_the_mode_and_the_tools() {
    let s = settings();
    let args = argv(&s, "review this", None);
    assert_eq!(args[0], "-p");
    assert_eq!(args[1], "review this");
    let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| i + 1);
    assert_eq!(&args[at("--output-format").expect("a format")], "json");
    let schema_at = at("--json-schema").expect("the schema");
    let parsed: Value = serde_json::from_str(&args[schema_at]).expect("the schema is JSON");
    assert_eq!(
        parsed,
        schema(),
        "the whole verdict schema, as one argument"
    );
    assert_eq!(
        &args[at("--permission-mode").expect("a mode")],
        DEFAULT_PERMISSION_MODE
    );
    let tools_at = at("--allowedTools").expect("the tools");
    assert_eq!(&args[tools_at..], &DEFAULT_TOOLS[..]);
    assert!(
        !args.iter().any(|a| a == "--resume"),
        "a review nobody has run before resumes nothing: {args:?}"
    );
    // A project's own tools replace the default list rather than joining it.
    let mut own = s.clone();
    own.allowed_tools = vec!["Read".into(), "Bash(git diff:*)".into()];
    let args = argv(&own, "p", None);
    let tools_at = args.iter().position(|a| a == "--allowedTools").unwrap() + 1;
    assert_eq!(&args[tools_at..], &["Read", "Bash(git diff:*)"]);
}

/// A re-review continues the conversation the last run left: `--resume <id>` before the tool list,
/// because `--allowedTools` would otherwise swallow the id as another tool name.
#[test]
fn a_review_with_a_conversation_resumes_it() {
    let argv = argv(&settings(), "the next head", Some("cc-7"));
    let resume = argv.iter().position(|a| a == "--resume").expect("--resume");
    assert_eq!(argv[resume + 1], "cc-7");
    let tools = argv
        .iter()
        .position(|a| a == "--allowedTools")
        .expect("the tools");
    assert!(resume < tools, "the id is not a tool: {argv:?}");
}

/// The docs' result object, read as the queue's verdict: the id a re-review resumes and the
/// decision with its evidence.
#[test]
fn a_verdict_in_structured_output_is_the_queues_verdict() {
    let out = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,
        "result":"I read the change.","session_id":"cc-1",
        "structured_output":{"decision":"reject","reasons":["the retry is unbounded"],
        "files":["crates/x.rs"],"commands":["git diff base...head"]}}"#;
    let answer = read_stdout(out).expect("a verdict");
    assert_eq!(answer.session_id.as_deref(), Some("cc-1"));
    assert_eq!(answer.verdict.decision, Decision::Reject);
    assert_eq!(answer.verdict.reasons, vec!["the retry is unbounded"]);
    assert_eq!(answer.verdict.looked_at.files, vec!["crates/x.rs"]);
    assert_eq!(
        answer.verdict.looked_at.commands,
        vec!["git diff base...head"]
    );
}

/// A result without `structured_output` is a FAILED review and never a pass. The two shapes that
/// matter are an explicit error and a plain prose answer; both are refused, and both keep the run's
/// own words so the operator can see what came instead.
#[test]
fn a_result_without_structured_output_is_an_error_never_a_pass() {
    for out in [
        r#"{"type":"result","is_error":true,"result":"invalid api key","session_id":"cc-2"}"#,
        r#"{"type":"result","is_error":false,"result":"Looks good to me!","session_id":"cc-3"}"#,
    ] {
        let why = read_stdout(out).expect_err("no verdict");
        assert!(why.contains("structured_output"), "{why}");
    }
    let errored = read_stdout(r#"{"is_error":true,"result":"invalid api key"}"#).unwrap_err();
    assert!(
        errored.contains("invalid api key"),
        "the run's own sentence is kept: {errored}"
    );
    // Not JSON at all is the same failure, said differently - and never a default.
    assert!(read_stdout("claude: command not found").is_err());
    assert!(read_stdout("").is_err());
}

/// A decision word outside the set is refused rather than read as the nearest one: a typo must not
/// land a branch, and `accept` is the word that would cost the most.
#[test]
fn a_decision_outside_the_set_is_refused_by_name() {
    let out = r#"{"structured_output":{"decision":"looks fine","reasons":[]}}"#;
    let why = read_stdout(out).expect_err("a word outside the set");
    assert!(why.contains("looks fine"), "{why}");
    for word in Decision::ALL.iter().map(|d| d.as_str()) {
        assert!(why.contains(word), "the sentence names `{word}`: {why}");
    }
    let none = r#"{"structured_output":{"reasons":["x"]}}"#;
    assert!(read_stdout(none).unwrap_err().contains("`decision`"));
}

/// `--output-format json` prints one object, but a run started by hand or on a stream leaves the
/// result among other lines: the newest object that carries the answer is the one read.
#[test]
fn the_answer_is_read_out_of_a_stream_and_not_only_a_single_object() {
    let out = "starting up\n{\"type\":\"system\",\"session_id\":\"cc-9\"}\n\
               {\"type\":\"result\",\"session_id\":\"cc-9\",\
               \"structured_output\":{\"decision\":\"accept\",\"reasons\":[\"done\"],\
               \"files\":[],\"commands\":[\"git log\"]}}\n";
    let answer = read_stdout(out).expect("the last object");
    assert_eq!(answer.verdict.decision, Decision::Accept);
    assert_eq!(answer.session_id.as_deref(), Some("cc-9"));
}
