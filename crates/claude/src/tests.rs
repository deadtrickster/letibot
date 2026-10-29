//! The reader's tests, on a fixture transcript written by hand.
//!
//! **The fixture is a file, not a mock**, because the two things rule 4 promises — the
//! bytes and the mtime are the same after a read as before it — can only be measured on a
//! real file. Every line of it is a shape taken from a real transcript on this box
//! (`~/.claude/projects/-Users-dead-Projects-letibot/*.jsonl`), including the ones this
//! build does not read.

use super::*;
use letibot_transcript::SegmentEdge;

/// A throwaway transcript directory: `<tmp>/letibot-claude-<tag>-<pid>/<project>/<id>.jsonl`.
/// A **directory** and not one file, because the listing walks projects and the id is
/// found by name — both are part of what is being tested.
fn fixture(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("letibot-claude-{tag}-{}-{}", std::process::id(), {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(p.join("proj-a")).unwrap();
    p
}

const UUID: &str = "11111111-2222-3333-4444-555555555555";

/// **The whole mapping in one transcript**, in the order a conversation writes it:
/// the operator's words, a thought, an answer with a call, the call's result, a recap, a
/// compaction summary, a sidechain line, and a block this build does not read.
///
/// The timestamps are distinct and deliberately not monotone-with-the-clock: nothing here
/// may read a clock, so the assertion is that each row carries ITS OWN line's time.
fn written(tag: &str) -> (PathBuf, PathBuf) {
    let dir = fixture(tag);
    let path = dir.join("proj-a").join(format!("{UUID}.jsonl"));
    let lines = [
        // 1. The operator's own words.
        r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"11111111-2222-3333-4444-555555555555","timestamp":"2026-10-07T12:28:34.023Z","cwd":"/home/dead/Projects/x","gitBranch":"main","version":"2.1.285","origin":{"kind":"human"},"promptSource":"typed","message":{"role":"user","content":"make the tests pass"}}"#,
        // 2. A thought, with a signature the source keeps and this build ignores.
        r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"11111111-2222-3333-4444-555555555555","timestamp":"2026-10-07T12:28:36.500Z","message":{"model":"claude-opus-5-5","id":"msg_1","role":"assistant","content":[{"type":"thinking","thinking":"the failing test is about paths","signature":"CAQSpAU"}]}}"#,
        // 3. An answer, and the call it makes — one block per line, as the source writes it.
        r#"{"type":"assistant","uuid":"a2","parentUuid":"a1","sessionId":"11111111-2222-3333-4444-555555555555","timestamp":"2026-10-07T12:28:37.000Z","message":{"model":"claude-opus-5-5","id":"msg_1","role":"assistant","content":[{"type":"text","text":"Running the suite."},{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        // 4. The result, in a later `user` line, paired by `tool_use_id`.
        r#"{"type":"user","uuid":"u2","parentUuid":"a2","sessionId":"11111111-2222-3333-4444-555555555555","timestamp":"2026-10-07T12:29:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"test result: ok. 3 passed","is_error":false}]}}"#,
        // 5. The model's own recap, said to an operator who stepped away.
        r#"{"type":"system","subtype":"away_summary","uuid":"s1","parentUuid":"u2","sessionId":"11111111-2222-3333-4444-555555555555","timestamp":"2026-10-07T12:47:29.030Z","content":"The suite is green; next is the migration.","isMeta":false}"#,
        // 6. The continuation summary after a compaction.
        r#"{"type":"summary","summary":"A conversation about making the tests pass.","leafUuid":"u2"}"#,
        // 7. A subagent's branch, inline (the older spelling of `subagents/`).
        r#"{"type":"user","isSidechain":true,"agentId":"ac0ffee","uuid":"sc1","parentUuid":"s1","timestamp":"2026-10-07T12:48:00.000Z","message":{"role":"user","content":"find every linux-only assumption"}}"#,
        r#"{"type":"assistant","isSidechain":true,"agentId":"ac0ffee","uuid":"sc2","parentUuid":"sc1","timestamp":"2026-10-07T12:48:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"looking"}]}}"#,
        // 8. A block this build does not read.
        r#"{"type":"assistant","uuid":"a3","parentUuid":"sc2","timestamp":"2026-10-07T12:49:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use_summary","text":"something new"}]}}"#,
        // 9. A line type this build does not read at all.
        r#"{"type":"something-new","sessionId":"11111111-2222-3333-4444-555555555555","timestamp":"2026-10-07T12:49:30.000Z"}"#,
        // 10. The spend side-channel, and its cumulative spelling.
        r#"{"type":"cost-state","sessionId":"11111111-2222-3333-4444-555555555555","totalCostUSD":1.5,"modelUsage":{"claude-opus-5-5":{"inputTokens":100,"outputTokens":20,"thinkingTokens":5,"cacheReadInputTokens":30,"cacheCreationInputTokens":7}}}"#,
        r#"{"type":"cost-state","sessionId":"11111111-2222-3333-4444-555555555555","totalCostUSD":2.25,"modelUsage":{"claude-opus-5-5":{"inputTokens":200,"outputTokens":40,"thinkingTokens":9,"cacheReadInputTokens":60,"cacheCreationInputTokens":14}}}"#,
    ];
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    (dir, path)
}

fn kinds(rows: &[Row]) -> Vec<&'static str> {
    rows.iter()
        .map(|r| match &r.item {
            TranscriptItem::User { .. } => "user",
            TranscriptItem::Assistant { tool_calls, .. } if !tool_calls.is_empty() => "call",
            TranscriptItem::Assistant { .. } => "assistant",
            TranscriptItem::Reasoning { .. } => "reasoning",
            TranscriptItem::ToolResult { .. } => "tool_result",
            TranscriptItem::SegmentMark { .. } => "mark",
            TranscriptItem::System { .. } => "system",
        })
        .collect()
}

/// **The mapping, in order, with the provenance and the times.**
#[test]
fn every_shape_maps_in_order_with_its_own_time_and_its_speaker() {
    let (dir, _path) = written("map");
    let src = Source::open(&dir).unwrap();

    let mut rows: Vec<Row> = Vec::new();
    let mut scraps: Vec<Scrap> = Vec::new();
    let mut progress: Vec<(u64, u64)> = Vec::new();
    let report = src
        .read(UUID, &mut |e| match e {
            Event::Progress { bytes, total } => progress.push((bytes, total)),
            Event::Row(r) => rows.push(r),
            Event::Scrap(s) => scraps.push(s),
        })
        .unwrap();

    // The order is the file's, and the shape is the table in the module doc.
    assert_eq!(
        kinds(&rows),
        vec![
            "user",        // 1 the operator
            "reasoning",   // 2 the thought
            "assistant",   // 3 "Running the suite."
            "call",        // 3 the Bash call, its own row
            "tool_result", // 4 the result, paired by id
            "assistant",   // 5 the away summary
            "assistant",   // 6 the continuation summary
        ],
        "one or two rows per line, in the file's order: {rows:?}"
    );

    // Rule 3: every row carries its own line's time, and nothing called `now`.
    assert_eq!(
        rows[0].ts_ms, 1_791_376_114_023,
        "the operator's line, to the ms"
    );
    assert_eq!(rows[1].ts_ms, 1_791_376_116_500);
    assert_eq!(rows[3].ts_ms, 1_791_376_117_000);
    assert_eq!(rows[4].ts_ms, 1_791_376_140_000);
    assert_eq!(rows[5].ts_ms, 1_791_377_249_030, "the away summary's own");
    // The `summary` line has no `timestamp` in the source, and the row says so with 0
    // rather than borrowing the previous row's clock.
    assert_eq!(
        rows[6].ts_ms, 0,
        "a line with no time gets none, never a guess"
    );

    // The call and the result are the same call, and the result carries the call's name.
    let TranscriptItem::Assistant { tool_calls, .. } = &rows[3].item else {
        panic!(
            "a tool_use is an Assistant row carrying the call: {:?}",
            rows[3]
        );
    };
    assert_eq!(tool_calls[0].id, "toolu_01");
    assert_eq!(tool_calls[0].name, "Bash");
    assert_eq!(tool_calls[0].arguments, r#"{"command":"cargo test"}"#);
    let TranscriptItem::ToolResult {
        call_id,
        name,
        outcome,
        payload,
        ..
    } = &rows[4].item
    else {
        panic!("a tool_result is its own row: {:?}", rows[4]);
    };
    assert_eq!(call_id, "toolu_01");
    assert_eq!(
        name, "Bash",
        "the name comes from the call, the only place it is"
    );
    assert_eq!(*outcome, ToolOutcome::Ok);
    assert!(payload.contains("3 passed"), "{payload}");

    // The thought is a Reasoning row, and its signature is not carried as text.
    let TranscriptItem::Reasoning { text, .. } = &rows[1].item else {
        panic!("a thinking block is a Reasoning row: {:?}", rows[1]);
    };
    assert_eq!(text, "the failing test is about paths");

    // Rule 1: the row's speaker is the source's own declaration.
    let TranscriptItem::User { speaker, parts } = &rows[0].item else {
        panic!("the operator's line is a User row: {:?}", rows[0]);
    };
    assert_eq!(*speaker, Speaker::Operator);
    assert_eq!(parts.len(), 1);

    // **The sidechain produced no rows and said so once.** Both its lines are skipped,
    // and the sentence names the branch.
    assert_eq!(report.sidechains, 1);
    assert_eq!(report.sidechain_lines, 2);
    let side = scraps
        .iter()
        .find(|s| s.tag == "isSidechain")
        .expect("a sidechain branch is named where it begins");
    assert!(side.said.contains("ac0ffee"), "{}", side.said);
    assert!(side.said.contains("NOT this conversation"), "{}", side.said);
    assert_eq!(side.line, 7, "at the branch's first line");

    // The unknown block and the unknown line type are said once each, and counted.
    assert_eq!(
        report.unknown_types.get("assistant:tool_use_summary"),
        Some(&1)
    );
    assert_eq!(report.unknown_types.get("something-new"), Some(&1));
    assert!(
        scraps
            .iter()
            .any(|s| s.tag == "something-new" && s.said.contains("does not read")),
        "{:?}",
        scraps.iter().map(|s| &s.tag).collect::<Vec<_>>()
    );

    // Rule 2: the spend is the LAST cumulative `cost-state`, not the sum of both.
    assert_eq!(report.spend.cost, 2.25);
    assert_eq!(report.spend.tokens.input, 200);
    assert_eq!(report.spend.tokens.reasoning, 9);
    assert_eq!(report.spend.tokens.cache_write, 14);
    assert_eq!(report.spend.tokens.total(), 200 + 40 + 9 + 60 + 14);

    // The progress counter is the file's own bytes, ending at the denominator.
    assert_eq!(progress.first().unwrap().1, report.bytes);
    assert_eq!(progress.last().unwrap().0, report.bytes);
    assert_eq!(report.lines, 12);

    let summary = report.summary();
    assert!(summary.contains("imported 7 row(s)"), "{summary}");
    assert!(summary.contains("another provider's units"), "{summary}");
    assert!(summary.contains("subagent branch(es)"), "{summary}");
    assert!(summary.contains("continuation summar"), "{summary}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **Rule 4, measured on the file**: the bytes and the mtime are the same after a read as
/// before it. A read that wrote anything — a WAL, a lock, a normalisation — would move one.
#[test]
fn reading_leaves_the_transcript_exactly_as_it_was() {
    let (dir, path) = written("untouched");
    let before = std::fs::read(&path).unwrap();
    let before_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

    let src = Source::open(&dir).unwrap();
    let _ = src.sessions().unwrap();
    let _ = src.tree(UUID).unwrap();
    let _ = src.directory(UUID).unwrap();
    let report = src.read(UUID, &mut |_| {}).unwrap();
    assert!(report.rows > 0);

    let after = std::fs::read(&path).unwrap();
    let after_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(before, after, "the transcript's bytes must not change");
    assert_eq!(
        before_mtime, after_mtime,
        "the transcript's mtime must not change"
    );
    // And nothing new appeared beside it.
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec![format!("{UUID}.jsonl")], "{names:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The listing a person needs to find the uuid without opening the JSONL.**
#[test]
fn the_listing_names_the_id_the_mtime_the_size_the_model_and_the_first_ask() {
    let (dir, _path) = written("list");
    let src = Source::open(&dir).unwrap();
    let list = src.sessions().unwrap();
    assert_eq!(list.len(), 1);
    let s = &list[0];
    assert_eq!(s.id, UUID);
    assert_eq!(s.project, "proj-a");
    assert_eq!(s.first, "make the tests pass");
    assert_eq!(s.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(s.cwd.as_deref(), Some("/home/dead/Projects/x"));
    assert_eq!(s.git_branch.as_deref(), Some("main"));
    assert_eq!(s.version.as_deref(), Some("2.1.285"));
    assert!(s.bytes > 0 && s.mtime_ms > 0, "{s:?}");

    // The two forms the CLI prints: a line a person reads, and a tsv a script reads.
    let line = s.line();
    assert!(line.starts_with(&format!("cc-{UUID}")), "{line}");
    assert!(line.contains("claude-opus-5-5"), "{line}");
    assert!(line.contains("make the tests pass"), "{line}");
    let tsv = s.tsv();
    assert_eq!(tsv.split('\t').count(), 6, "{tsv}");
    assert!(tsv.starts_with(&format!("cc-{UUID}\t")), "{tsv}");

    // Rule 1's sentence, for the session's header.
    let origin = s.origin();
    assert!(origin.contains("imported from Claude Code"), "{origin}");
    assert!(origin.contains(UUID), "{origin}");
    assert!(origin.contains("/home/dead/Projects/x"), "{origin}");
    assert!(origin.contains("claude-opus-5-5"), "{origin}");
    assert!(origin.contains("2.1.285"), "{origin}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A missing directory and a missing id are two different sentences** — the pair the
/// operator asked to be told apart for opencode, and the same rule here.
#[test]
fn a_missing_directory_and_a_missing_id_are_different_sentences() {
    let absent = std::env::temp_dir().join("letibot-claude-definitely-absent-dir");
    let _ = std::fs::remove_dir_all(&absent);
    let err = match Source::open(&absent) {
        Ok(_) => panic!("a directory that is not there must not open"),
        Err(e) => e,
    };
    assert!(matches!(err, ImportError::NoDirectory { .. }), "{err}");
    let said = format!("{err}");
    assert!(
        said.contains("no Claude Code transcript directory"),
        "{said}"
    );
    assert!(
        said.contains("not the session being absent"),
        "the two facts are told apart in the sentence: {said}"
    );

    let (dir, _path) = written("missing");
    let src = Source::open(&dir).unwrap();
    let err = src
        .read("99999999-0000-0000-0000-000000000000", &mut |_| {})
        .unwrap_err();
    assert!(matches!(err, ImportError::NoSuchSession { .. }), "{err}");
    let said = format!("{err}");
    assert!(said.contains("has no transcript"), "{said}");
    assert!(
        said.contains("--list-claude"),
        "the sentence names the way to find one: {said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **An empty id is not a session**, and asking for one must not walk every project
/// looking for `/.jsonl`.
#[test]
fn an_empty_id_is_refused_rather_than_matched() {
    let (dir, _path) = written("empty-id");
    let src = Source::open(&dir).unwrap();
    assert!(matches!(
        src.tree("").unwrap_err(),
        ImportError::NoSuchSession { .. }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A tool result whose call this build never saw** — a transcript that begins after a
/// compaction, or a branch whose call is in another file. Counted, and named `tool`, so
/// the card says something rather than nothing.
#[test]
fn an_unpaired_result_is_counted_and_named() {
    let dir = fixture("unpaired");
    let path = dir.join("proj-a").join("aa.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-10-07T12:00:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_gone","content":"orphaned","is_error":true}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let src = Source::open(&dir).unwrap();
    let mut rows = Vec::new();
    let report = src
        .read("aa", &mut |e| {
            if let Event::Row(r) = e {
                rows.push(r);
            }
        })
        .unwrap();
    assert_eq!(report.unpaired_results, 1);
    let TranscriptItem::ToolResult { name, outcome, .. } = &rows[0].item else {
        panic!("{:?}", rows[0]);
    };
    assert_eq!(name, "tool");
    assert!(matches!(outcome, ToolOutcome::Failed { .. }), "{outcome:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The frames a `user` line arrives in**, and who each one belongs to: the operator's
/// own shell act and paste are theirs, a job's settlement and a subagent's hand-back are
/// another agent's, and Claude Code's plumbing is nobody's.
#[test]
fn a_frame_is_carried_with_its_author_and_plumbing_is_said_not_drawn() {
    let cases: &[(&str, Option<(&str, Speaker)>)] = &[
        // `!` is this head's own spelling for the operator running a command.
        (
            "<bash-input> gh pr merge 1</bash-input>",
            Some(("! gh pr merge 1", Speaker::Operator)),
        ),
        // A paste is the person's words, with the frame taken off — the closing tag
        // carries the id in the real spelling.
        (
            "<pasted_content id=\"17da\">\n\nhello there\n</pasted_content id=\"17da\">",
            Some(("hello there", Speaker::Operator)),
        ),
        // A job's settlement: this head appends one as `User` with `speaker: Agent`.
        (
            "<task-notification>\n<task-id>j1</task-id>\n<status>failed</status>\n</task-notification>",
            Some((
                "<task-id>j1</task-id>\n<status>failed</status>",
                Speaker::Agent,
            )),
        ),
        // A subagent's hand-back says in its own text that it is not the person's.
        (
            "<agent-message from=\"ab7a\">done</agent-message>",
            Some(("done", Speaker::Agent)),
        ),
        // Plumbing: nobody's words.
        (
            "<local-command-caveat>run directly</local-command-caveat>",
            None,
        ),
        // A frame this build does not read is said, not invented.
        ("<brand-new-frame>what</brand-new-frame>", None),
    ];
    for (text, want) in cases {
        match (classify(text, Speaker::Operator), want) {
            (Said::Words { text, speaker }, Some((t, s))) => {
                assert_eq!(&text, t, "{text}");
                assert_eq!(&speaker, s, "{text}");
            }
            (Said::Machinery(_), None) | (Said::UnknownFrame(_), None) => {}
            (got, _) => panic!("{text} → {:?}", std::mem::discriminant(&got)),
        }
    }

    // Plumbing in FRONT of the person's words is stripped, and the words are kept.
    match classify(
        "<local-command-caveat>ctx</local-command-caveat>\n\nactually do the other thing",
        Speaker::Operator,
    ) {
        Said::Words { text, speaker } => {
            assert_eq!(text, "actually do the other thing");
            assert_eq!(speaker, Speaker::Operator);
        }
        other => panic!("{:?}", std::mem::discriminant(&other)),
    }

    // Prose that merely begins with `<` is prose, not a frame.
    assert!(matches!(
        classify("<3 thanks", Speaker::Operator),
        Said::Words { .. }
    ));
}

/// **The speaker is read off the source's own declaration**, not guessed from the text —
/// the counts in `declared_speaker`'s doc are from this box's transcripts.
#[test]
fn the_speaker_is_the_sources_declaration() {
    let of = |raw: &str| declared_speaker(&serde_json::from_str::<Value>(raw).unwrap());
    // The person.
    assert_eq!(of(r#"{"origin":{"kind":"human"}}"#), Speaker::Operator);
    assert_eq!(of(r#"{"promptSource":"typed"}"#), Speaker::Operator);
    assert_eq!(of(r#"{"promptSource":"queued"}"#), Speaker::Operator);
    // Another agent, or the harness continuing itself.
    assert_eq!(
        of(r#"{"origin":{"kind":"task-notification"}}"#),
        Speaker::Agent
    );
    assert_eq!(of(r#"{"origin":{"kind":"peer"}}"#), Speaker::Agent);
    assert_eq!(
        of(r#"{"origin":{"kind":"auto-continuation"}}"#),
        Speaker::Agent
    );
    assert_eq!(of(r#"{"promptSource":"system"}"#), Speaker::Agent);
    // `isMeta` wins over both: a meta line is Claude Code's own text.
    assert_eq!(
        of(r#"{"isMeta":true,"origin":{"kind":"human"}}"#),
        Speaker::Agent
    );
    // Absent is the operator — the reading an old row gets, and what a head drew for it.
    assert_eq!(of("{}"), Speaker::Operator);
}

/// **The ISO-8601 parse**, which is rule 3's only clock: a time it cannot read is `None`,
/// never a guess.
#[test]
fn timestamps_parse_to_the_millisecond_and_never_to_a_guess() {
    // 2026-10-07T12:28:34.023Z, checked against `date -u -r` on this box.
    assert_eq!(
        parse_ts("2026-10-07T12:28:34.023Z"),
        Some(1_791_376_114_023)
    );
    assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_ts("2026-10-07T12:28:34Z"), Some(1_791_376_114_000));
    assert_eq!(
        parse_ts("2026-10-07T12:28:34.023456Z"),
        Some(1_791_376_114_023)
    );
    // An offset is ISO-8601 and is honoured: 14:28:34+02:00 is the same instant.
    assert_eq!(
        parse_ts("2026-10-07T14:28:34.023+02:00"),
        parse_ts("2026-10-07T12:28:34.023Z")
    );
    assert_eq!(
        parse_ts("2026-10-07T07:28:34.023-05:00"),
        parse_ts("2026-10-07T12:28:34.023Z")
    );
    // A leap day is a day.
    assert_eq!(
        parse_ts("2024-02-29T00:00:00Z").map(|ms| ms / 86_400_000),
        Some(19_782)
    );
    // Everything else is no time at all.
    for bad in [
        "",
        "2026-10-07",
        "not a time",
        "2026-13-01T00:00:00Z",
        "2026-10-07T25:00:00Z",
    ] {
        assert_eq!(parse_ts(bad), None, "{bad}");
    }
    // A clock read at import time would make this fail: the value is the file's, and the
    // same string parses to the same number however long the test runs.
    assert_eq!(
        parse_ts("2026-10-07T12:28:34.023Z"),
        Some(1_791_376_114_023)
    );
}

/// **A transcript being written right now** ends in a half line, and that is said rather
/// than reported as a parse failure of the format.
#[test]
fn a_half_written_last_line_is_named_as_that() {
    let dir = fixture("partial");
    let path = dir.join("proj-a").join("bb.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-10-07T12:00:00.000Z","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"wor"#
        ),
    )
    .unwrap();
    let src = Source::open(&dir).unwrap();
    let mut said = Vec::new();
    let report = src
        .read("bb", &mut |e| {
            if let Event::Scrap(s) = e {
                said.push(s.said);
            }
        })
        .unwrap();
    assert_eq!(report.malformed, 1);
    assert!(
        said.iter().any(|s| s.contains("still writing")),
        "a half line is a conversation being written, not a broken format: {said:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A subagent transcript beside the session is counted and not walked** — the `Task`
/// call and its report are already rows in the parent, and the child's working
/// conversation is its own session (see the module doc's `isSidechain` section).
#[test]
fn the_subagent_transcripts_beside_a_session_are_counted_and_left_alone() {
    let (dir, _path) = written("subagents");
    let subs = dir.join("proj-a").join(UUID).join("subagents");
    std::fs::create_dir_all(&subs).unwrap();
    for n in 0..3 {
        std::fs::write(
            subs.join(format!("agent-a{n}.jsonl")),
            r#"{"type":"user","isSidechain":true,"agentId":"a0","message":{"role":"user","content":"go"}}"#,
        )
        .unwrap();
    }
    let src = Source::open(&dir).unwrap();
    let tree = src.tree(UUID).unwrap();
    assert_eq!(tree.subagents, 3, "counted");
    // Listed as a session of its own: no. The listing is transcripts, one level down.
    assert_eq!(src.sessions().unwrap().len(), 1);

    let report = src.read(UUID, &mut |_| {}).unwrap();
    assert_eq!(report.subagents, 3);
    assert!(
        report
            .summary()
            .contains("3 subagent transcript(s) sit beside this one"),
        "{}",
        report.summary()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reader against the real directory on this box, when there is one. Ignored by
/// default so the suite is deterministic; run it with
/// `cargo test -p letibot-claude -- --ignored` on the box that has the transcripts.
#[test]
#[ignore = "reads the operator's real ~/.claude/projects"]
fn the_real_transcripts_read_and_nothing_is_written() {
    let Some(dir) = default_projects_dir() else {
        eprintln!("no HOME; nothing to read");
        return;
    };
    let Ok(src) = Source::open(&dir) else {
        eprintln!("no transcripts at {}; nothing to read", dir.display());
        return;
    };
    let list = src.sessions().unwrap();
    eprintln!("{} transcript(s) under {}", list.len(), dir.display());
    let Some(biggest) = list.iter().max_by_key(|s| s.bytes) else {
        return;
    };
    eprintln!(
        "the biggest: {} ({}), {}",
        biggest.id,
        bytes_human(biggest.bytes),
        biggest.origin()
    );
    let before = std::fs::read(&biggest.path).unwrap();
    let mut rows = 0u64;
    let report = src
        .read(&biggest.id, &mut |e| {
            if let Event::Row(_) = e {
                rows += 1;
            }
        })
        .unwrap();
    assert_eq!(rows, report.rows);
    assert_eq!(
        std::fs::read(&biggest.path).unwrap(),
        before,
        "the transcript must be byte-identical after a read"
    );
    eprintln!("{}", report.summary());
}

/// `SegmentEdge` is used only by the compaction arm; this keeps the import honest if the
/// mark's shape ever moves.
#[test]
fn a_compaction_boundary_is_a_mark_that_renders_nothing() {
    let dir = fixture("compact");
    let path = dir.join("proj-a").join("cc.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"system","subtype":"compact_boundary","uuid":"s1","timestamp":"2026-10-07T12:00:00.000Z","content":"Conversation compacted","compactMetadata":{"trigger":"auto","preTokens":968900,"postTokens":15710}}"#,
            "\n"
        ),
    )
    .unwrap();
    let src = Source::open(&dir).unwrap();
    let mut rows = Vec::new();
    let report = src
        .read("cc", &mut |e| {
            if let Event::Row(r) = e {
                rows.push(r);
            }
        })
        .unwrap();
    assert_eq!(report.compactions, 1);
    let TranscriptItem::SegmentMark {
        label, kind, edge, ..
    } = &rows[0].item
    else {
        panic!("{:?}", rows[0]);
    };
    assert_eq!(*edge, SegmentEdge::Open);
    assert_eq!(kind, "compaction");
    assert!(label.contains("968900 → 15710"), "{label}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The idempotency rule** — a resumed session does not import a second time, because a
/// second copy of the conversation appended to a session that already holds it is the one
/// way an import can corrupt a transcript. Tested here, where the rule lives, rather than
/// through a daemon that would need a vocabulary to say it.
#[test]
fn a_fresh_cc_session_imports_and_a_resumed_one_does_not() {
    let id = "e90320f4-b550-4aa0-aa11-1c2673df88e8";
    assert_eq!(import_target(false, &format!("cc-{id}")), Some(id));
    // **The resume is the rule**: the same id, resumed, imports nothing.
    assert_eq!(import_target(true, &format!("cc-{id}")), None);
    // Other namespaces are other packages' business.
    assert_eq!(import_target(false, &format!("oc-{id}")), None);
    assert_eq!(import_target(false, "s-1793010881933039000"), None);
    // `cc-` with nothing behind it names no transcript.
    assert_eq!(import_target(false, "cc-"), None);
}
