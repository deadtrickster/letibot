//! The writer's tests: the conservative subset, the round trip back through the reader,
//! and the append path's three refusals and its way back.
//!
//! Every assertion about a backup compares **bytes**, never a size: a short write, a full
//! disk and a page of zeroes all have the right size, and only one of them is a backup.

use super::*;
use crate::{Event, Source};
use letibot_transcript::{ReasoningField, ToolCall, UserPart};

fn tmp(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "letibot-claude-ho-{tag}-{}-{}",
        std::process::id(),
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            N.fetch_add(1, Ordering::Relaxed)
        }
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A conversation as this head holds it: the operator, a thought, an answer with a call,
/// the call's result, and a row this session wrote itself.
fn conversation() -> Vec<(u64, TranscriptItem)> {
    vec![
        (
            1_791_376_114_023,
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: "make the tests pass".into(),
                }],
                speaker: Speaker::Operator,
            },
        ),
        (
            1_791_376_116_500,
            TranscriptItem::Reasoning {
                text: "the failing test is about paths".into(),
                field: ReasoningField::ReasoningContent,
                truncated: false,
            },
        ),
        (
            1_791_376_117_000,
            TranscriptItem::Assistant {
                text: "Running the suite.".into(),
                tool_calls: vec![ToolCall {
                    id: "toolu_01".into(),
                    name: "Bash".into(),
                    arguments: r#"{"command":"cargo test"}"#.into(),
                }],
                truncated: false,
            },
        ),
        (
            1_791_376_140_000,
            TranscriptItem::ToolResult {
                call_id: "toolu_01".into(),
                name: "Bash".into(),
                outcome: ToolOutcome::Ok,
                payload: "test result: ok. 3 passed".into(),
                edit: None,
                origin: None,
                media: None,
            },
        ),
        (
            1_791_377_249_030,
            TranscriptItem::User {
                parts: vec![UserPart::Text {
                    text: "job 3 finished".into(),
                }],
                speaker: Speaker::Agent,
            },
        ),
    ]
}

fn meta() -> Meta {
    Meta::fresh(Path::new("/home/dead/Projects/x"), Some("main".into()))
}

/// **The conservative subset, field by field** — and the rows it refuses to fake.
#[test]
fn a_handover_writes_the_subset_the_documentation_documents() {
    let dir = tmp("write");
    let out = dir.join("handover");
    let meta = meta();
    let w = write_handover(&conversation(), &meta, &out).unwrap();
    assert_eq!(w.lines, 4, "five rows in, one thought skipped: {w:?}");
    assert_eq!(w.skipped.thinking, 1);
    assert!(
        w.skipped
            .said()
            .contains("signature only the provider can mint")
    );

    let text = std::fs::read_to_string(&w.jsonl).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("every line must be JSON"))
        .collect();
    assert_eq!(lines.len(), 4);

    // Every line carries the documented fields.
    let mut parent: Option<String> = None;
    for (i, l) in lines.iter().enumerate() {
        for k in ["type", "uuid", "sessionId", "cwd", "version", "timestamp"] {
            assert!(l.get(k).is_some(), "line {i} has no `{k}`: {l}");
        }
        assert!(l["type"] == "user" || l["type"] == "assistant", "{l}");
        // The chain is linear and whole.
        assert_eq!(
            l["parentUuid"].as_str().map(str::to_string),
            parent,
            "line {i}'s parentUuid is the previous line's uuid"
        );
        parent = l["uuid"].as_str().map(str::to_string);
        // The time is the row's own, in the source's spelling.
        if i == 0 {
            assert_eq!(l["timestamp"], "2026-10-07T12:28:34.023Z", "{l}");
        }
    }
    // The session id is OURS and fresh, not a copy of anything.
    assert_eq!(lines[0]["sessionId"], meta.session_id.as_str());
    assert_eq!(
        lines[0]["version"],
        format!("letibot-{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(lines[0]["cwd"], "/home/dead/Projects/x");
    assert_eq!(lines[0]["gitBranch"], "main");

    // The assistant line carries its text AND its call, as one content array.
    let content = lines[1]["message"]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "toolu_01");
    assert_eq!(content[1]["input"]["command"], "cargo test");

    // The result answers it by id, in a `user` line.
    let result = &lines[2];
    assert_eq!(result["type"], "user");
    let block = &result["message"]["content"][0];
    assert_eq!(block["type"], "tool_result");
    assert_eq!(block["tool_use_id"], "toolu_01");
    assert_eq!(block["is_error"], false);

    // A row this session wrote itself is marked in Claude Code's own spelling for
    // *text the person did not type*.
    let agent = &lines[3];
    assert_eq!(agent["isMeta"], true);
    assert_eq!(agent["promptSource"], "system");
    assert_eq!(agent["message"]["content"][0]["text"], "job 3 finished");
    // ...and the operator's row is not.
    assert!(lines[0].get("isMeta").is_none(), "{}", lines[0]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The round trip**: what the writer writes, the reader reads back as the same
/// conversation. Two crates' halves of one format, checked against each other rather than
/// each against its own idea of it.
#[test]
fn what_the_writer_writes_the_reader_reads_back() {
    let dir = tmp("roundtrip");
    let project = dir.join("proj");
    let meta = meta();
    let w = write_handover(&conversation(), &meta, &project).unwrap();

    let src = Source::open(&dir).unwrap();
    let mut rows = Vec::new();
    let report = src
        .read(&meta.session_id, &mut |e| {
            if let Event::Row(r) = e {
                rows.push(r);
            }
        })
        .unwrap();
    // **Five rows, from four lines**: the writer put the assistant's text and its call on
    // one line (one content array, as the format allows) and the reader maps a block at a
    // time, so a line with two blocks comes back as two rows. That is the pair of
    // contracts meeting, not a discrepancy.
    assert_eq!(rows.len(), 5, "the four lines the handover carried");
    assert_eq!(report.rows, 5);
    // The reader found the file by the id the handover named.
    assert_eq!(report.malformed, 0, "{}", report.summary());

    // Same order, same speakers, same call, same result.
    assert!(matches!(
        &rows[0].item,
        TranscriptItem::User {
            speaker: Speaker::Operator,
            ..
        }
    ));
    let TranscriptItem::Assistant {
        text, tool_calls, ..
    } = &rows[1].item
    else {
        panic!("{:?}", rows[1]);
    };
    assert_eq!(text, "Running the suite.");
    assert!(tool_calls.is_empty(), "the text block is its own row");
    let TranscriptItem::Assistant { tool_calls, .. } = &rows[2].item else {
        panic!("{:?}", rows[2]);
    };
    assert_eq!(tool_calls[0].id, "toolu_01");
    let TranscriptItem::ToolResult {
        call_id, payload, ..
    } = &rows[3].item
    else {
        panic!("{:?}", rows[3]);
    };
    assert_eq!(call_id, "toolu_01");
    assert!(payload.contains("3 passed"));
    assert!(matches!(
        &rows[4].item,
        TranscriptItem::User {
            speaker: Speaker::Agent,
            ..
        }
    ));

    // The times came back as the times that went in, which is what the format's own
    // spelling buys.
    assert_eq!(rows[0].ts_ms, 1_791_376_114_023);
    assert_eq!(rows[2].ts_ms, 1_791_376_117_000);
    assert_eq!(rows[4].ts_ms, 1_791_377_249_030);

    // A row with no time gets no `timestamp` and is counted, rather than being stamped
    // with a clock this crate made up.
    let mut untimed = conversation();
    untimed[0].0 = 0;
    let w2 = write_handover(&untimed, &meta, &project).unwrap();
    assert_eq!(w2.skipped.no_time, 1);
    let first = std::fs::read_to_string(&w2.jsonl).unwrap();
    let first: Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
    assert!(first.get("timestamp").is_none(), "{first}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = w;
}

/// **The Markdown half**, which is the one a Claude release cannot break: who spoke, a
/// tool call on one line, and a result collapsed to its first line and a count.
#[test]
fn the_markdown_says_who_spoke_and_collapses_a_call_to_one_line() {
    let dir = tmp("md");
    let mut conv = conversation();
    conv[3].1 = TranscriptItem::ToolResult {
        call_id: "toolu_01".into(),
        name: "Bash".into(),
        outcome: ToolOutcome::Ok,
        payload: "line one\nline two\nline three".into(),
        edit: None,
        origin: None,
        media: None,
    };
    let w = write_handover(&conv, &meta(), &dir).unwrap();
    let md = std::fs::read_to_string(&w.markdown).unwrap();
    assert!(
        md.starts_with("# A letibot conversation, handed to Claude Code"),
        "{md}"
    );
    assert!(md.contains("**operator**\n\nmake the tests pass"), "{md}");
    assert!(md.contains("**assistant**\n\nRunning the suite."), "{md}");
    assert!(
        md.contains("> tool call `Bash` — `{\"command\":\"cargo test\"}`"),
        "a call is one line: {md}"
    );
    assert!(
        md.contains("> tool result `Bash` — ok: line one … (3 lines)"),
        "a result is collapsed and says how much it hid: {md}"
    );
    // A row this session wrote is not drawn as the operator's words.
    assert!(
        md.contains("**letibot (the session's own voice, not the operator's)**"),
        "{md}"
    );
    // The thought is in the readable half even though it is not in the JSONL: a person
    // pasting this wants the gist, and a Markdown file claims nothing about a signature.
    assert!(
        md.contains("> _thinking_ — the failing test is about paths"),
        "{md}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the append path ---------------------------------------------------------------

const ORIGINAL: &str = concat!(
    r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s-1","timestamp":"2026-10-07T12:00:00.000Z","cwd":"/w","gitBranch":"main","version":"2.1.285","message":{"role":"user","content":"hi"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s-1","timestamp":"2026-10-07T12:00:01.000Z","cwd":"/w","gitBranch":"main","version":"2.1.285","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
    "\n"
);

fn transcript(tag: &str) -> (PathBuf, PathBuf) {
    let dir = tmp(tag);
    let path = dir.join("s-1.jsonl");
    std::fs::write(&path, ORIGINAL).unwrap();
    (dir, path)
}

/// **One line, and a backup that holds the exact original bytes.**
#[test]
fn a_note_appends_one_line_and_the_backup_is_the_original_byte_for_byte() {
    let (dir, path) = transcript("append");
    let now = 1_791_376_114_023u64; // 2026-10-07T12:28:34.023Z
    let got = append_note(&path, "picked this up in letibot", now, &NoOneLive).unwrap();

    // The backup is named for the moment, and holds the original bytes exactly.
    let name = got
        .backup
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(name, "s-1.jsonl.bak-20261007-122834", "{name}");
    assert_eq!(
        std::fs::read(&got.backup).unwrap(),
        ORIGINAL.as_bytes(),
        "the backup must be the original, byte for byte"
    );

    // One line went on the end, and it is the conservative one.
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with(ORIGINAL),
        "the original lines are untouched"
    );
    let last = after.lines().last().unwrap();
    let line: Value = serde_json::from_str(last).unwrap();
    assert_eq!(line["type"], "user");
    assert_eq!(line["message"]["content"], "picked this up in letibot");
    assert!(
        line["message"]["content"].is_string(),
        "a plain string, not a block list"
    );
    assert_eq!(
        line["parentUuid"], "a1",
        "the file's last line is the parent"
    );
    assert_eq!(
        line["sessionId"], "s-1",
        "the file's own session, not a new one"
    );
    assert_eq!(line["cwd"], "/w");
    assert_eq!(line["gitBranch"], "main");
    assert_eq!(line["version"], "2.1.285", "the file's own version, copied");
    assert_eq!(line["timestamp"], "2026-10-07T12:28:34.023Z");
    assert_eq!(line["isMeta"], true, "the note is not the operator's words");
    assert_eq!(got.bytes as usize, last.len() + 1, "one complete line");

    // **A second append makes a second backup, and never overwrites the first.** The
    // pristine original is always the oldest one.
    let second = append_note(&path, "and again", now + 1000, &NoOneLive).unwrap();
    assert_ne!(second.backup, got.backup);
    assert_eq!(
        std::fs::read(&got.backup).unwrap(),
        ORIGINAL.as_bytes(),
        "the first backup still holds the pristine original"
    );
    assert_eq!(
        std::fs::read(&second.backup).unwrap(),
        after.as_bytes(),
        "the second holds the state before the second append"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **No live Claude, no append** — and the refusal comes before the backup, so a refused
/// append leaves no debris either.
#[test]
fn an_append_refuses_while_a_claude_is_live_for_the_project_and_writes_nothing() {
    struct Someone;
    impl Live for Someone {
        fn live_for(&self, _: &Path, _: &str) -> std::result::Result<Vec<String>, String> {
            Ok(vec!["4242: claude --resume /w".into()])
        }
    }
    let (dir, path) = transcript("live");
    let err = append_note(&path, "hello", 1_791_376_114_023, &Someone).unwrap_err();
    let said = format!("{err}");
    assert!(said.contains("refusing to append"), "{said}");
    assert!(
        said.contains("4242"),
        "the refusal names the process: {said}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        ORIGINAL.as_bytes(),
        "nothing was written"
    );
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["s-1.jsonl"],
        "no backup was left behind: {names:?}"
    );

    // And a check that could not RUN is a refusal too: not knowing is not the same fact
    // as nobody being there.
    struct Broken;
    impl Live for Broken {
        fn live_for(&self, _: &Path, _: &str) -> std::result::Result<Vec<String>, String> {
            Err("`ps` could not be run".into())
        }
    }
    let err = append_note(&path, "hello", 1_791_376_114_023, &Broken).unwrap_err();
    assert!(format!("{err}").contains("could not be checked"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), ORIGINAL.as_bytes());
    let _ = std::fs::remove_dir_all(&dir);
}

/// **No backup, no write** — a directory that cannot be written to refuses the append and
/// leaves the transcript exactly as it was.
#[test]
fn a_backup_that_cannot_be_made_refuses_the_append() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, path) = transcript("nobackup");
    // A read-only directory: the file is readable and appending to it would work, and the
    // backup cannot be created. That is precisely the case the rule is about.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let err = append_note(&path, "hello", 1_791_376_114_023, &NoOneLive).unwrap_err();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(format!("{err}").contains("Permission denied"), "{err}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        ORIGINAL.as_bytes(),
        "a failed backup means the transcript is untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The way back**: `revert` restores the file byte for byte from a named backup, and
/// leaves the backup where it is so the revert can be reverted.
#[test]
fn a_revert_puts_the_transcript_back_byte_for_byte() {
    let (dir, path) = transcript("revert");
    let got = append_note(&path, "a note to undo", 1_791_376_114_023, &NoOneLive).unwrap();
    assert_ne!(std::fs::read(&path).unwrap(), ORIGINAL.as_bytes());

    let bytes = revert(&path, &got.backup, &NoOneLive).unwrap();
    assert_eq!(bytes as usize, ORIGINAL.len());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        ORIGINAL.as_bytes(),
        "byte for byte, not merely the same size"
    );
    assert!(
        got.backup.exists(),
        "the backup stays, so a revert can be reverted"
    );

    // A revert while somebody is live is refused, for the same reason the append is.
    struct Someone;
    impl Live for Someone {
        fn live_for(&self, _: &Path, _: &str) -> std::result::Result<Vec<String>, String> {
            Ok(vec!["1: claude".into()])
        }
    }
    let err = revert(&path, &got.backup, &Someone).unwrap_err();
    assert!(format!("{err}").contains("refusing to revert"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A transcript somebody was killed in the middle of writing** is not appended to: the
/// note would land inside a broken record.
#[test]
fn a_half_written_last_line_is_refused_rather_than_appended_to() {
    let dir = tmp("partial");
    let path = dir.join("s-1.jsonl");
    std::fs::write(
        &path,
        format!("{ORIGINAL}{{\"type\":\"assistant\",\"uuid\":\"a2\""),
    )
    .unwrap();
    let err = append_note(&path, "hello", 1_791_376_114_023, &NoOneLive).unwrap_err();
    assert!(format!("{err}").contains("incomplete"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The id and the two time spellings, on values checked against `date -u`.
#[test]
fn ids_are_fresh_v4_uuids_and_the_stamps_are_utc() {
    let a = new_session_id();
    let b = new_session_id();
    assert_eq!(a.len(), 36, "{a}");
    assert_eq!(a.as_bytes()[14], b'4', "version 4: {a}");
    assert!(
        matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
        "variant: {a}"
    );
    assert_ne!(a, b, "a handover's id must not collide with anything");
    assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{a}");

    assert_eq!(iso_ts(1_791_376_114_023), "2026-10-07T12:28:34.023Z");
    assert_eq!(stamp_utc(1_791_376_114_023), "20261007-122834");
    assert_eq!(iso_ts(0), "1970-01-01T00:00:00.000Z");
    // The two halves of the clock agree: what `iso_ts` writes, `parse_ts` reads.
    for ms in [
        0u64,
        1_791_376_114_023,
        1_709_164_800_000,
        1_000_000_000_001,
    ] {
        assert_eq!(crate::parse_ts(&iso_ts(ms)), Some(ms), "{ms}");
    }
}

/// `PsLive` is the real check and is exercised here against this box: it must not report
/// *this* test process as a Claude for a directory nobody is in. It is a smoke test, not a
/// proof — the refusals above are proved with a fake, which is the only way to prove a
/// refusal about a process a test cannot start.
#[test]
fn the_real_live_check_answers_and_does_not_claim_a_claude_in_an_empty_directory() {
    let dir = tmp("pslive");
    let quiet = Path::new("/var/empty");
    match PsLive.live_for(quiet, "a-session-nobody-has") {
        Ok(v) => {
            // Whatever else is running on this box, none of it is in /var/empty for this
            // session id.
            assert!(v.is_empty(), "unexpected: {v:?}");
        }
        // `ps` itself unavailable is a legitimate answer, and the append treats it as live.
        Err(e) => eprintln!("no `ps` here: {e}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
