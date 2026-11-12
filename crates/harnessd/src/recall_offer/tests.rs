//! The offer against a fixture index and fixture words: no store, no model, no harness. What this
//! module does not do is the reading - it does not index a transcript, it does not put the line in
//! the request, and it says nothing unless one of its two triggers earned it.

use super::*;
use letibot_tokencore::store::RecallUnit;
use letibot_transcript::{ToolOutcome, UserPart};

/// One unit of an older conversation, as the index keeps it.
fn unit(id: &str, session: &str, at_ms: i64, text: &str) -> RecallUnit {
    RecallUnit {
        unit: id.into(),
        session: session.into(),
        title: "the store split".into(),
        at_ms,
        text: text.into(),
    }
}

/// The index a fixture project has: the etcd turn, and enough others that `idf` has something to be
/// computed over - a corpus of one makes every term equally rare and no floor discriminates there.
fn index() -> Vec<RecallUnit> {
    vec![
        unit(
            "s-etcd#t0#2",
            "s-etcd",
            1_760_000_000_000,
            "build the etcd export as reference-format archives, one file per key prefix, so the \
             restore can be replayed key by key",
        ),
        unit(
            "s-etcd#t0#8",
            "s-etcd",
            1_760_000_100_000,
            "the postgres migration backfills in batches of a thousand rows and the lock is \
             released between them",
        ),
        unit(
            "s-tui#t0#1",
            "s-tui",
            1_760_000_200_000,
            "the model card filters as a line is typed, and a digit narrows instead of marking a \
             row",
        ),
    ]
}

/// A tool result that mentions a phrase no person said - the half of the subject the trigger must
/// never read.
fn result(text: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "call_1".into(),
        name: "read".into(),
        outcome: ToolOutcome::Ok,
        payload: text.into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    }
}

/// A call the model made.
fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "call_2".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

/// The line the model is handed, as text.
fn text_of(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => panic!("a user-side row, not {other:?}"),
    }
}

/// A seat that has the two tools, and one that has not.
fn seated() -> Offers {
    let tools = ["transcript".to_string(), "read".to_string()];
    Offers::new().with_tools(tools.iter())
}

/// The Done-when's first clause: the operator asks "remember how we built the etcd archives?" and
/// the hint names `transcript`, the term to match, and the session the turn is in - at the tail,
/// and in no row.
#[test]
fn a_back_reference_is_answered_with_the_call_and_where_the_turn_was() {
    let mut offers = seated();
    offers.units(index(), BTreeMap::new());
    assert!(offers.compose(
        "remember how we built the etcd archives?",
        "remember how we built the etcd archives?",
    ));
    let tail = offers.tail();
    assert_eq!(tail.len(), 1, "one line");
    let text = text_of(&tail[0]);
    assert!(text.contains("[recall offer"), "{text}");
    // The heaviest shared terms tie here, so the LATER word of the question is the match - and
    // the other one is named in the same line.
    assert!(
        text.contains("`transcript` with match: \"archives\""),
        "{text}"
    );
    assert!(
        text.contains("etcd"),
        "and the other term that scored it: {text}"
    );
    assert!(text.contains("history: true"), "{text}");
    assert!(text.contains("s-etcd"), "the source session: {text}");
    assert!(text.contains("the store split"), "and its title: {text}");
    // 2025-10-09 is what 1_760_000_000_000 ms is, and the hint names the day and not a delta.
    assert!(text.contains("2025-10-09"), "{text}");
    assert!(
        text.contains("gone from the next one"),
        "it says it is this round's: {text}"
    );
}

/// The second clause: a prompt with no back-reference and nothing over the floor gets no hint. The
/// sourdough shares no term with any unit, so the score is zero and the floor is never reached.
#[test]
fn nothing_is_said_without_a_back_reference_or_a_score_over_the_floor() {
    let mut offers = seated();
    offers.units(index(), BTreeMap::new());
    let sourdough = "the sourdough starter wants equal weights of flour and water and a warm place";
    assert!(!offers.compose(sourdough, sourdough));
    assert!(offers.tail().is_empty());
}

/// The third: on a box with no embedder the lexical path still offers, and the two floors are what
/// makes that true. One shared word against one long unit scores between [`TRIGGER_FLOOR`] and
/// [`OFFER_FLOOR`], so the same words are offered only when the person referred back.
#[test]
fn the_words_are_offered_on_the_trigger_and_not_without_it() {
    let subject = "we should check the sourdough starter and the coffee grinder and the postgres \
                   thing this weekend";
    let mut quiet = seated();
    quiet.units(index(), BTreeMap::new());
    assert!(
        !quiet.compose(subject, subject),
        "a shared word is not enough on its own"
    );
    let mut asked = seated();
    asked.units(index(), BTreeMap::new());
    assert!(
        asked.compose(subject, "idk, maybe we've done that already"),
        "the same words, with the person referring back"
    );
    assert_eq!(
        asked.last_open_tool(),
        "transcript",
        "one turn is a read, so the hint is a read"
    );
}

/// A back-reference earns the pointer even with nothing indexed at all: the project's history is
/// cold, which is not an answer, and the person's own longest word is the best `match` there is.
#[test]
fn a_back_reference_with_nothing_indexed_still_points_at_the_tool() {
    let mut offers = seated();
    offers.units(Vec::new(), BTreeMap::new());
    assert!(offers.compose(
        "remember when we rotated the kafka credentials?",
        "remember when we rotated the kafka credentials?"
    ));
    let text = text_of(&offers.tail()[0]);
    assert!(
        text.contains("`transcript` with match: \"credentials\""),
        "{text}"
    );
    assert!(!text.contains("session"), "no session to name: {text}");
}

/// A term that is all over the project is a fold through a subagent, not one turn to read: the hint
/// reaches for `digest` and asks the person's own question. The fourth unit is what keeps the
/// corpus from being three copies of one text and only it, where every term has `idf` of zero and
/// nothing scores at all.
#[test]
fn a_match_all_over_the_project_reaches_for_digest() {
    let wide: Vec<RecallUnit> = [
        "etcd archive rotation",
        "etcd archive rotation",
        "etcd archive rotation",
        "the sourdough starter wants equal weights of flour and water",
    ]
    .iter()
    .enumerate()
    .map(|(i, text)| {
        let s = format!("s-{i}");
        unit(&format!("{s}#t0#1"), &s, 1_760_000_000_000 + i as i64, text)
    })
    .collect();
    let mut offers = seated();
    offers.units(wide, BTreeMap::new());
    assert!(offers.compose("the etcd archive rotation", "the etcd archive rotation"));
    let text = text_of(&offers.tail()[0]);
    assert!(text.contains("`digest` with match: \""), "{text}");
    assert!(
        text.contains("question: \"the etcd archive rotation\""),
        "the person's own words are the question, and not a composed one: {text}"
    );
}

/// The take-up: the round's own `transcript` call, matched on the term the hint named, and a log
/// line either way - which is what the floor is tuned from.
#[test]
fn the_call_the_hint_named_is_the_take_up_and_it_is_logged() {
    let logs = std::env::temp_dir().join(format!("harnessd-recall-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&logs);
    std::fs::create_dir_all(&logs).expect("a log directory");
    let mut offers = seated();
    offers.units(index(), BTreeMap::new());
    assert!(offers.compose(
        "remember how we built the etcd archives?",
        "remember how we built the etcd archives?"
    ));
    let settled = offers
        .settle(
            &logs,
            "s-me",
            &[call("transcript", r#"{"match": "etcd", "history": true}"#)],
        )
        .cloned()
        .expect("the round's offer");
    assert!(settled.taken, "the match overlapped the hint's term");
    let lines = std::fs::read_to_string(logs.join(LOG)).expect("the log exists");
    assert_eq!(lines.lines().count(), 1);
    let row: serde_json::Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
    assert_eq!(row["taken"], true);
    assert_eq!(row["unit"], "s-etcd#t0#2");
    assert_eq!(row["match"], "archives");
    assert_eq!(row["tool"], "transcript");
    let _ = std::fs::remove_dir_all(&logs);
}

/// An offer the round did not take is logged as untaken and is not made again: the second time is a
/// nag, and the first was the line the model was told to act on.
#[test]
fn an_untaken_offer_is_logged_and_is_not_made_twice() {
    let logs = std::env::temp_dir().join(format!("harnessd-recall-untaken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&logs);
    std::fs::create_dir_all(&logs).expect("a log directory");
    let mut offers = seated();
    offers.units(index(), BTreeMap::new());
    let words = "remember how we built the etcd archives?";
    assert!(offers.compose(words, words));
    let settled = offers
        .settle(&logs, "s-me", &[call("read", r#"{"path": "src/main.rs"}"#)])
        .cloned()
        .expect("the round's offer");
    assert!(!settled.taken, "a read of a file is not a look back");
    assert!(!offers.compose(words, words), "and it is not said again");
    let _ = std::fs::remove_dir_all(&logs);
}

/// The memory is the log, so a session that restarts does not offer the same unit again.
#[test]
fn the_log_is_the_memory_of_what_was_already_offered() {
    let logs = std::env::temp_dir().join(format!("harnessd-recall-memory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&logs);
    std::fs::create_dir_all(&logs).expect("a log directory");
    let words = "remember how we built the etcd archives?";
    let mut first = seated();
    first.units(index(), BTreeMap::new());
    assert!(first.compose(words, words));
    first.settle(&logs, "s-me", &[]);
    let mut second = seated();
    second.units(index(), BTreeMap::new());
    second.recall(&logs, "s-me");
    assert!(
        !second.compose(words, words),
        "the next session of this id has already been told"
    );
    // And another session is told: the memory is per session, not per project.
    let mut other = seated();
    other.units(index(), BTreeMap::new());
    other.recall(&logs, "s-other");
    assert!(other.compose(words, words));
    let _ = std::fs::remove_dir_all(&logs);
}

/// A seat without `transcript` hears nothing, however loudly the words refer back: a hint naming a
/// tool the session has not got is a round spent on a refusal.
#[test]
fn a_seat_without_the_tool_hears_nothing() {
    let tools = ["read".to_string(), "bash".to_string()];
    let mut offers = Offers::new().with_tools(tools.iter());
    assert!(!offers.seated());
    offers.units(index(), BTreeMap::new());
    let words = "remember how we built the etcd archives?";
    assert!(!offers.compose(words, words));
}

/// The trigger reads the person's own row and never a tool result: a payload saying "before" is not
/// somebody referring back.
#[test]
fn the_trigger_is_the_operators_words_and_not_a_tool_result() {
    let items = vec![
        TranscriptItem::ToolResult {
            call_id: "call_1".into(),
            name: "bash".into(),
            outcome: ToolOutcome::Ok,
            payload: "before you commit, run the gate".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
        TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "what does the gate script do".into(),
            }],
        },
    ];
    let said = operator_text(&items);
    assert!(said.contains("gate script"), "{said}");
    assert_eq!(refers_back(&said), None, "nothing refers back: {said}");
    // And a harness line is not the operator's either.
    let harness = vec![TranscriptItem::User {
        speaker: Speaker::Agent,
        parts: vec![UserPart::Text {
            text: "remember the note offer".into(),
        }],
    }];
    assert!(operator_text(&harness).is_empty());
}

/// The closed set, on word boundaries: `against` is not `again`, and each phrase that names the
/// past is one.
#[test]
fn the_refer_back_set_matches_words_and_not_substrings() {
    assert_eq!(refers_back("did we already do that?"), Some("did we"));
    assert_eq!(refers_back("Like Last Time"), Some("last time"));
    assert_eq!(refers_back("that thing we tried"), Some("that thing"));
    assert_eq!(refers_back("run it again"), Some("again"));
    assert_eq!(refers_back("weigh it against the other one"), None);
    assert_eq!(refers_back("the rememberer is a machine"), None);
    assert_eq!(refers_back("read the parser"), None);
}

/// A unit is the person's turn and the reply it got: the model's reasoning and the tool's payload
/// are not a unit, because a unit is what was SAID about the work.
#[test]
fn a_unit_is_a_persons_turn_and_the_reply_it_got() {
    let rows: Vec<(u32, String, String, i64)> = vec![
        (
            0,
            "user".into(),
            serde_json::to_string(&TranscriptItem::User {
                speaker: Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "build the etcd export".into(),
                }],
            })
            .unwrap(),
            10,
        ),
        (
            1,
            "tool_result".into(),
            serde_json::to_string(&result("a payload about nothing")).unwrap(),
            11,
        ),
        (
            2,
            "assistant".into(),
            serde_json::to_string(&TranscriptItem::Assistant {
                text: "done, as reference-format archives".into(),
                tool_calls: Vec::new(),
                truncated: false,
            })
            .unwrap(),
            12,
        ),
        (
            3,
            "user".into(),
            serde_json::to_string(&TranscriptItem::User {
                speaker: Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "now the restore".into(),
                }],
            })
            .unwrap(),
            20,
        ),
    ];
    let built = units(&rows, "s-1", "s-1#t0", "the store split");
    assert_eq!(built.len(), 2, "two human turns, two units");
    assert_eq!(built[0].unit, "s-1#t0#0");
    assert_eq!(built[0].at_ms, 10);
    assert!(
        built[0].text.contains("build the etcd export"),
        "{:?}",
        built[0]
    );
    assert!(
        built[0].text.contains("reference-format archives"),
        "the reply is in the unit: {:?}",
        built[0]
    );
    assert!(!built[0].text.contains("a payload about nothing"));
    assert_eq!(built[1].unit, "s-1#t0#3");
    // A fork keeps its parent's rows, so the id names the transcript as well as the seq.
    assert_eq!(units(&rows, "s-1", "s-1#t1", "")[0].unit, "s-1#t1#0");
}

/// A unit whose text is longer than the cap is cut on a character boundary, and a subject with
/// Cyrillic in it is not a panic.
#[test]
fn a_cut_inside_a_character_does_not_panic() {
    let long = "привет ".repeat(400);
    assert_eq!(cut(&long, 1_200).len(), 1_200);
    assert!(cut(&long, 1_200).is_char_boundary(cut(&long, 1_200).len()));
    assert_eq!(one_line("a\n\tb  \"c\"", 200), "a b 'c'");
}

/// The date is the day the turn was written, and the epoch is the day it is.
#[test]
fn the_date_is_the_day_the_turn_was_written() {
    assert_eq!(date_of(0), "1970-01-01");
    assert_eq!(date_of(1_760_000_000_000), "2025-10-09");
    assert_eq!(date_of(-86_400_000), "1969-12-31");
}

/// The stored vectors carry a paraphrase no word of which the unit has: the index's own vectors are
/// what makes the meaning reachable, and a unit without one is scored on its words.
#[test]
fn a_stored_vector_carries_a_paraphrase_no_word_of_which_the_unit_has() {
    let data = std::env::temp_dir().join(format!("harnessd-recall-planted-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("a data dir");
    let tier = letibot_embeddings::testing::planted(
        &data,
        &[
            &["vessel", "container"],
            &["rollout", "deployment"],
            &["sails", "ships"],
        ],
    );
    let units = vec![unit(
        "s-ship#t0#1",
        "s-ship",
        1_760_000_000_000,
        "a vessel sails into production",
    )];
    let vector = tier.query(&units[0].text, now_ms()).expect("a vector");
    let mut vectors = BTreeMap::new();
    vectors.insert(units[0].unit.clone(), vector);
    // The words share nothing, so without the vector there is nothing to offer.
    let mut words_only = seated();
    words_only.units(units.clone(), BTreeMap::new());
    assert!(!words_only.compose("container deployment", "container deployment"));
    let mut offers = seated().with_tier(Some(tier));
    offers.units(units, vectors);
    assert!(
        offers.compose("container deployment", "container deployment"),
        "the meaning is what earns it"
    );
    assert!(
        text_of(&offers.tail()[0]).contains("match: \"deployment\""),
        "a paraphrase shares no term, so the person's own most specific word is the match"
    );
    let _ = std::fs::remove_dir_all(&data);
}

impl Offers {
    /// The tool of the round's open offer, for the tests above: the open half is private, and the
    /// tail is what a caller reads.
    fn last_open_tool(&self) -> &'static str {
        self.open.as_ref().map(|o| o.tool).unwrap_or_default()
    }
}
