//! The census for structural eviction, over conversations built by hand: the summary turn's own
//! prompt is not a model call, so every claim here is a value.

use super::*;
use letibot_transcript::{Speaker, ToolCall, UserPart};

fn body(tag: &str, bytes: usize) -> String {
    let mut s = format!("{tag}\n");
    while s.len() < bytes {
        s.push_str("the quick brown fox jumps over the lazy dog\n");
    }
    s
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// One call as the model proposed it, with the arguments spelled as the tool spells them.
fn call(id: &str, name: &str, arguments: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }],
        truncated: false,
    }
}

fn result(id: &str, name: &str, outcome: ToolOutcome, payload: String) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: id.into(),
        name: name.into(),
        outcome,
        payload,
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

/// A read of one file, from a call to its body.
fn read(id: &str, path: &str, bytes: usize) -> [TranscriptItem; 2] {
    [
        call(id, "read", &format!(r#"{{"path":"{path}"}}"#)),
        result(
            id,
            "read",
            ToolOutcome::Ok,
            body(&format!("{path} {id}"), bytes),
        ),
    ]
}

fn span(parts: Vec<[TranscriptItem; 2]>) -> Vec<TranscriptItem> {
    let mut out = vec![user("start")];
    for p in parts {
        out.extend(p);
    }
    out.push(user("carry on"));
    out
}

/// A span where the last part is the exchange being worked on: the `user` turn before it is what
/// makes it the newest one, and the parts before that are old.
fn span_ending_in_hand(parts: Vec<[TranscriptItem; 2]>) -> Vec<TranscriptItem> {
    let last = parts.len();
    let mut out = vec![user("start")];
    for (i, p) in parts.into_iter().enumerate() {
        if i + 1 == last {
            out.push(user("now this one"));
        }
        out.extend(p);
    }
    out
}

/// A store with a body in it per id, minting the ids itself: so a test can say that the map's id
/// is the one the body went in under.
#[derive(Default)]
struct Kept(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

impl BodyStore for Kept {
    fn keep(&self, _tool: &str, body: &[u8]) -> Option<String> {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let id = format!("k{}", held.len());
        held.insert(id.clone(), String::from_utf8_lossy(body).into_owned());
        Some(id)
    }
}

impl Kept {
    fn of(&self, id: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }
}

/// The reduction with a store that keeps everything, for the tests about which units go.
fn reduced(items: &[TranscriptItem], rows: &[String]) -> Eviction {
    reduce(items, rows, &Kept::default())
}

/// The payload of the result for that call, or `None` when the span does not hold it.
fn payload_of<'a>(items: &'a [TranscriptItem], id: &str) -> Option<&'a str> {
    items.iter().find_map(|i| match i {
        TranscriptItem::ToolResult {
            call_id, payload, ..
        } if call_id == id => Some(payload.as_str()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// The classes
// ---------------------------------------------------------------------------

/// A file read and then written over: the body cannot be the file any more, so it goes - and the
/// write that superseded it is still whole, because that is the current state.
#[test]
fn a_read_a_later_write_supersedes_is_evicted() {
    let items = span_ending_in_hand(vec![
        read("c1", "src/a.rs", 4_000),
        [
            call("c2", "write", r#"{"path":"src/a.rs"}"#),
            result("c2", "write", ToolOutcome::Ok, body("wrote it", 600)),
        ],
    ]);
    let out = reduced(&items, &[]);
    let unit = out
        .units
        .iter()
        .find(|u| u.call_id == "c1")
        .unwrap_or_else(|| panic!("the superseded read must go: {:?}", out.units));
    assert!(
        matches!(&unit.why, Why::Superseded { .. }),
        "a write of the same path supersedes the read: {:?}",
        unit.why
    );
    assert_eq!(
        unit.gone + payload_of(&out.items, "c1").unwrap().len(),
        payload_of(&items, "c1").unwrap().len(),
        "what it cost is the body less the stub"
    );
    assert!(
        payload_of(&out.items, "c1").unwrap().contains("[evicted"),
        "a stub stands where the body was: {:?}",
        payload_of(&out.items, "c1")
    );
    assert!(
        payload_of(&out.items, "c2")
            .unwrap()
            .starts_with("wrote it"),
        "the call that superseded it is the current state and stays whole"
    );
}

/// The same read twice: the first body is the same bytes as the second, and the second is the
/// current one because it is in the exchange the session is inside.
#[test]
fn a_read_repeated_word_for_word_goes_and_the_last_one_stays() {
    let items = span_ending_in_hand(vec![
        read("c1", "src/a.rs", 4_000),
        read("c2", "src/a.rs", 4_000),
    ]);
    let out = reduced(&items, &[]);
    assert_eq!(
        out.units
            .iter()
            .map(|u| u.call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1"],
        "only the earlier of the two goes"
    );
    assert!(matches!(out.units[0].why, Why::Superseded { .. }));
    assert!(
        payload_of(&out.items, "c2")
            .unwrap()
            .starts_with("src/a.rs c2")
    );
}

/// Two disjoint windows of one file are not the same subject: neither supersedes the other, and
/// each is only a finished call.
#[test]
fn two_windows_of_one_file_are_not_one_subject() {
    let items = span(vec![
        [
            call(
                "c1",
                "read",
                r#"{"path":"src/a.rs","offset":1,"limit":200}"#,
            ),
            result("c1", "read", ToolOutcome::Ok, body("first window", 4_000)),
        ],
        [
            call(
                "c2",
                "read",
                r#"{"path":"src/a.rs","offset":201,"limit":200}"#,
            ),
            result("c2", "read", ToolOutcome::Ok, body("second window", 4_000)),
        ],
    ]);
    let out = reduced(&items, &[]);
    for unit in &out.units {
        assert_eq!(
            unit.why,
            Why::Finished,
            "a different window is a different body: {:?}",
            unit.why
        );
    }
}

/// A subagent that answered: its transcript is its own session, named in the stub.
#[test]
fn a_settled_subagent_report_goes_and_names_the_session() {
    let items = span(vec![
        [
            call("c1", "task_result", r#"{"task":"s-abc"}"#),
            result(
                "c1",
                "task_result",
                ToolOutcome::Ok,
                body("the report", 3_000),
            ),
        ],
        read("c2", "src/a.rs", 2_000),
    ]);
    let out = reduced(&items, &[]);
    let unit = out
        .units
        .iter()
        .find(|u| u.call_id == "c1")
        .unwrap_or_else(|| panic!("a settled subagent's report goes: {:?}", out.units));
    assert_eq!(
        unit.why,
        Why::Settled {
            child: "s-abc".into()
        }
    );
    let stub = payload_of(&out.items, "c1").unwrap();
    assert!(stub.contains("s-abc"), "the address survives: {stub}");
    assert!(!stub.contains("the report"), "the body does not: {stub}");
}

/// A finished call is the third class: nothing else superseded it, so it goes only once the two
/// classes that lose nothing have gone.
#[test]
fn the_classes_go_in_order() {
    let items = span_ending_in_hand(vec![
        [
            call("c1", "bash", r#"{"command":"cargo test"}"#),
            result("c1", "bash", ToolOutcome::Ok, body("test output", 3_000)),
        ],
        read("c2", "src/a.rs", 2_000),
        read("c3", "src/a.rs", 2_000),
    ]);
    let out = reduced(&items, &[]);
    let classes: Vec<u8> = out.units.iter().map(|u| u.why.class()).collect();
    assert_eq!(
        classes,
        vec![0, 2],
        "the superseded read goes before the finished call: {:?}",
        out.units
    );
    assert_eq!(out.units[0].call_id, "c2");
    assert_eq!(out.units[1].call_id, "c1");
}

// ---------------------------------------------------------------------------
// What is never evicted
// ---------------------------------------------------------------------------

/// The plan is the session's own statement of what is in hand, and a unit it names is kept whole -
/// by the path it spells, by the file's own name, or by the command it names.
#[test]
fn a_unit_the_plan_still_names_keeps_its_body() {
    let items = span(vec![
        read("c1", "crates/turn/src/evict.rs", 3_000),
        [
            call("c2", "bash", r#"{"command":"cargo test -p letibot-turn"}"#),
            result("c2", "bash", ToolOutcome::Ok, body("testing", 3_000)),
        ],
        [
            call("c3", "task_result", r#"{"task":"s-abc"}"#),
            result(
                "c3",
                "task_result",
                ToolOutcome::Ok,
                body("the report", 3_000),
            ),
        ],
    ]);
    let rows = vec![
        "wire the plan rows into the fold — see crates/turn/src/evict.rs".to_string(),
        "run cargo test -p letibot-turn and read the failure".to_string(),
        "collect s-abc's answer and land the branch".to_string(),
    ];
    let out = reduced(&items, &rows);
    assert!(
        out.units.is_empty(),
        "every one of these is named by a plan row: {:?}",
        out.units
    );
    assert_eq!(out.items, items, "the span is untouched");
}

/// The file's own name is how a plan row usually names it, so the basename of a path counts.
#[test]
fn a_plan_row_naming_the_file_by_its_name_keeps_it() {
    let items = span(vec![read("c1", "crates/turn/src/evict.rs", 3_000)]);
    let out = reduced(&items, &["finish evict.rs and its doc".to_string()]);
    assert!(out.units.is_empty(), "the basename is the name: {out:?}");
}

/// The newest exchange is the work in hand: a stub in the middle of it is a hole in the turn in
/// progress, and one this could not be put back.
#[test]
fn the_newest_exchange_is_left_whole() {
    let mut items = vec![user("start")];
    items.extend(read("c1", "src/old.rs", 4_000));
    items.push(user("now look at this one"));
    items.extend(read("c2", "src/new.rs", 4_000));
    let out = reduced(&items, &[]);
    assert_eq!(
        out.units
            .iter()
            .map(|u| u.call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1"],
        "the exchange the session is inside keeps its bodies"
    );
}

/// A failure's body IS the evidence - an error string is the one thing a record cannot re-derive -
/// and a backgrounded call has not finished at all.
#[test]
fn a_failure_and_a_running_call_keep_their_bodies() {
    let items = span(vec![
        [
            call("c1", "bash", r#"{"command":"cargo test"}"#),
            result(
                "c1",
                "bash",
                ToolOutcome::Failed {
                    reason: "exit 101".into(),
                },
                body("error[E0308]: mismatched types", 3_000),
            ),
        ],
        [
            call("c2", "bash", r#"{"command":"cargo build --release"}"#),
            result(
                "c2",
                "bash",
                ToolOutcome::Backgrounded {
                    handle: "j7".into(),
                    ran_for_ms: 1_000,
                    how: letibot_transcript::Backgrounding::Asked,
                    next: "job_output j7".into(),
                },
                body("building", 3_000),
            ),
        ],
    ]);
    let out = reduced(&items, &[]);
    assert!(out.units.is_empty(), "{:?}", out.units);
}

/// A body shorter than the floor is not worth a line of stub.
#[test]
fn a_short_body_is_not_worth_a_stub() {
    let items = span(vec![
        read("c1", "src/a.rs", 20),
        [
            call("c2", "read", r#"{"path":"src/a.rs"}"#),
            result("c2", "read", ToolOutcome::Ok, body("again", 20)),
        ],
    ]);
    let out = reduced(&items, &[]);
    assert!(out.units.is_empty(), "{:?}", out.units);
}

// ---------------------------------------------------------------------------
// What the record can still name
// ---------------------------------------------------------------------------

/// The fidelity gate reads paths and commands out of CALL arguments, so evicting bodies changes
/// nothing it can count: the addressable half of a unit was never the body.
#[test]
fn the_facts_the_gate_counts_survive_eviction() {
    let items = span(vec![
        read("c1", "src/a.rs", 3_000),
        read("c2", "src/a.rs", 3_000),
        [
            call("c3", "bash", r#"{"command":"cargo test -p letibot-turn"}"#),
            result("c3", "bash", ToolOutcome::Ok, body("ok", 3_000)),
        ],
    ]);
    let out = reduced(&items, &[]);
    assert!(!out.units.is_empty(), "the span has units to evict");
    let before = crate::fidelity::Facts::of(&items);
    let after = crate::fidelity::Facts::of(&out.items);
    assert_eq!(
        before, after,
        "every path and command the conversation held is still in the reduced span"
    );
    assert_eq!(before.files, vec!["src/a.rs".to_string()]);
    assert_eq!(
        before.commands,
        vec!["cargo test -p letibot-turn".to_string()]
    );
}

/// The stub is one line that says what went and why, and it never quotes the body.
#[test]
fn the_stub_is_one_line_that_says_what_went() {
    let items = span(vec![
        read("c1", "src/a.rs", 3_000),
        read("c2", "src/a.rs", 3_000),
    ]);
    let out = reduced(&items, &[]);
    let stub = payload_of(&out.items, "c1").unwrap();
    assert!(
        stub.lines().count() <= 3,
        "a stub is not a paragraph: {stub}"
    );
    assert!(stub.contains("src/a.rs"), "it names the subject: {stub}");
    assert!(stub.contains("c2"), "and what superseded it: {stub}");
    assert!(
        !stub.contains("the quick brown fox"),
        "the body must be gone: {stub}"
    );
    assert!(stub.len() <= STUB_ARGS + 200, "{}", stub.len());
}

/// A unit in the map is a unit whose body the store holds under that same name: the map is the
/// index of an eviction, and an index nobody can look up is a tombstone nobody can read.
#[test]
fn every_evicted_unit_is_named_in_the_map_and_kept() {
    let items = span_ending_in_hand(vec![
        read("c1", "src/a.rs", 4_000),
        read("c2", "src/a.rs", 4_000),
    ]);
    let kept = Kept::default();
    let out = reduce(&items, &[], &kept);
    let unit = out
        .units
        .first()
        .unwrap_or_else(|| panic!("the earlier read must go: {:?}", out.units));
    assert!(
        !unit.id.is_empty(),
        "a unit without an id is not addressable"
    );
    assert_eq!(
        kept.of(&unit.id).as_deref(),
        payload_of(&items, "c1"),
        "the store holds the body exactly, under the id the map names"
    );
    assert_eq!(
        unit.named, "src/a.rs",
        "the map says what the unit was about"
    );

    // The stub in the reduced span names the same id, so the summariser's own copy of the span
    // points at the same body the map does.
    let stub = payload_of(&out.items, "c1").unwrap();
    assert!(stub.contains(&unit.id), "the stub names the id: {stub}");

    let map = out.map();
    assert!(
        map.contains(&unit.id) && map.contains("read") && map.contains("src/a.rs"),
        "one line per unit, carrying the id, the kind and what it named: {map}"
    );
    assert_eq!(
        map.lines().filter(|l| l.starts_with("- ")).count(),
        out.units.len(),
        "exactly one line per unit: {map}"
    );
    assert!(map.contains("recall"), "and how to get it back: {map}");
}

/// A body the store will not take leaves the unit alone. An eviction whose body cannot be got back
/// is the hole the whole design refuses, so the store's refusal is the unit's answer.
#[test]
fn a_body_the_store_refuses_is_not_evicted() {
    struct Refuses;
    impl BodyStore for Refuses {
        fn keep(&self, _tool: &str, _body: &[u8]) -> Option<String> {
            None
        }
    }
    let items = span_ending_in_hand(vec![
        read("c1", "src/a.rs", 4_000),
        read("c2", "src/a.rs", 4_000),
    ]);
    let out = reduce(&items, &[], &Refuses);
    assert!(
        out.units.is_empty(),
        "nothing could be kept: {:?}",
        out.units
    );
    assert_eq!(out.items, items, "so the span is whole");
    assert_eq!(out.map(), "", "and there is no map to write");
}

/// The reduction is a value: the span handed in is not touched.
#[test]
fn the_span_handed_in_is_left_alone() {
    let items = span(vec![
        read("c1", "src/a.rs", 3_000),
        read("c2", "src/a.rs", 3_000),
    ]);
    let before = items.clone();
    let out = reduced(&items, &[]);
    assert_eq!(items, before, "the session's own items are not edited");
    assert_ne!(out.items, before, "and the reduced span is not it either");
    assert_eq!(out.gone(), out.units.iter().map(|u| u.gone).sum::<usize>());
}

/// A delivery opens the exchange in hand as a prompt does. A wake's turn begins with a `Notice`
/// row, so a span whose last message is a firing is the turn being worked on - and the read under
/// it must not be evicted out from under a live turn.
#[test]
fn a_notice_opens_the_exchange_in_hand() {
    let mut items = vec![
        user("start"),
        read("c1", "src/a.rs", 4_000)[0].clone(),
        read("c1", "src/a.rs", 4_000)[1].clone(),
        TranscriptItem::Notice {
            kind: letibot_transcript::NoticeKind::Monitor,
            text: "[monitor] `pr-watch` fired: the pull request was merged.".into(),
            facts: None,
        },
        read("c2", "src/b.rs", 4_000)[0].clone(),
        read("c2", "src/b.rs", 4_000)[1].clone(),
    ];
    // The same body twice: the first is superseded by nothing, the second would be the one this
    // rule protects, so make them the same path and let the rule decide which goes.
    items[4] = call("c2", "read", r#"{"path":"src/a.rs"}"#);
    items[5] = result("c2", "read", ToolOutcome::Ok, body("src/a.rs c2", 4_000));
    let out = reduce(&items, &[], &Kept::default());
    let first = out.units.iter().find(|u| u.call_id == "c1");
    assert!(
        first.is_some(),
        "the read before the notice is old and may go: {:?}",
        out.units
    );
    assert!(
        out.units.iter().all(|u| u.call_id != "c2"),
        "the read inside the turn the notice opened stays: {:?}",
        out.units
    );
}
