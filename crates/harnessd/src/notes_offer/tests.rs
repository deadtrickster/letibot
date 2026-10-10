//! The offer, against a fixture corpus and a fixture transcript — no model, no
//! network, no harness.
//!
//! The whole point of this module is what it does NOT do: it does not put the
//! note in the prompt, it does not put the offer in the log, and it says nothing
//! unless it is sure. Every one of those is a claim about a value that is either
//! absent or present, so every one of them is testable here.

use super::*;
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};

/// A workspace and a box-wide notes dir that remove themselves.
struct Fixture {
    ws: PathBuf,
    global: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base =
            std::env::temp_dir().join(format!("harnessd-offer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = base.join("ws");
        let global = base.join("global");
        std::fs::create_dir_all(ws.join(".letibot/notes")).expect("ws");
        std::fs::create_dir_all(&global).expect("global");
        Fixture { ws, global }
    }

    /// The one note the fixtures offer: an abstract and headings about the offer
    /// design, which is what the subject below is about.
    fn note(&self, stem: &str, abstract_: &str, headings: &[&str]) -> PathBuf {
        let mut text = format!("<!-- abstract: {abstract_} -->\n");
        for h in headings {
            text.push_str(&format!("\n## {h}\n\nbody text about {h}\n"));
        }
        let path = self.ws.join(".letibot/notes").join(format!("{stem}.md"));
        std::fs::write(&path, text).expect("fixture note");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(base) = self.ws.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

/// The person's turn: one prompt, as the transcript carries it.
fn prompt(text: &str) -> Vec<TranscriptItem> {
    vec![TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }]
}

/// A tool result, the other half of the subject.
fn result(text: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "call_1".into(),
        name: "read".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: text.into(),
        edit: None,
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

/// A subject squarely about the offer note.
const ABOUT: &str = "the offer must live at the tail of the request, out of the log, \
                    composed per turn, because re-injecting into the cached prefix \
                    invalidates every token after it. score over the corpus and tune the \
                    floor from the take-up log";

/// A subject about nothing in the fixture.
const UNRELATED: &str = "the sourdough starter wants equal weights of flour and water and a \
                         warm place for a day";

/// The corpus a fixture workspace has: the note under test, and enough others
/// that `idf` has something to be computed over — a corpus of one note is a
/// corpus where every term is equally rare, and no floor discriminates there.
fn corpus(f: &Fixture) -> PathBuf {
    f.note(
        "the-store-write-lock",
        "a store connection waits thirty seconds for the write lock and a busy handler holds \
         the wait",
        &["The lock", "The busy handler", "What the operator saw"],
    );
    f.note(
        "compaction-folds-the-first-half",
        "compaction folds the first half of a conversation into a summary and the tail is \
         clamped against the budget",
        &["What is folded", "The tail", "The budget"],
    );
    offer_note(f)
}

fn offer_note(f: &Fixture) -> PathBuf {
    f.note(
        "the-offer-and-the-tail",
        "an offer lives at the tail of the request and out of the log, because the cached prefix \
         must not move",
        &[
            "The take-up log",
            "Why a row cannot be un-said",
            "The floor",
        ],
    )
}

/// **The offer is composed at the tail, and it is not in the transcript.**
///
/// The design's sharper half: *"a row cannot be un-said; an assembly can be
/// recomposed"*. So the item the request carries is returned by [`Offers::tail`]
/// and is nowhere in the items it was composed from.
#[test]
fn the_offer_is_at_the_tail_and_never_in_the_transcript() {
    let f = Fixture::new("tail");
    let path = corpus(&f);
    let items = prompt(ABOUT);

    let mut offers = Offers::new();
    assert!(
        offers.compose(&f.ws, &f.global, "", &items),
        "the note earns a hint"
    );

    let tail = offers.tail();
    assert_eq!(tail.len(), 1, "one line, and one line only");
    let TranscriptItem::User { speaker, parts } = &tail[0] else {
        panic!("the tail is a user-side row: {:?}", tail[0]);
    };
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness's own line, never drawn as the operator's"
    );
    let UserPart::Text { text } = &parts[0] else {
        panic!("a text part");
    };
    assert!(
        text.contains(&path.display().to_string()),
        "the offer names the path a `read` can take: {text}"
    );
    assert!(
        text.contains("not the operator's"),
        "and says whose line it is: {text}"
    );
    assert!(
        text.contains("not in this prompt"),
        "it offers a fetch rather than making one: {text}"
    );
    assert!(
        !text.contains("cached prefix"),
        "and never quotes the note: {text}"
    );

    // **And the transcript is untouched.** The items handed in are exactly the
    // items that would be logged — that is what makes the offer provisional.
    assert_eq!(items.len(), 1, "no row was appended to the transcript");
    assert!(
        matches!(
            &items[0],
            TranscriptItem::User {
                speaker: Speaker::Operator,
                ..
            }
        ),
        "the person's own row is the only one there"
    );
}

/// **An offer nobody takes leaves no trace, and the next round says nothing.**
///
/// *"if it doesnt follow up with the note read - we can discard the offer from
/// context altogether"* — and the same note is not offered twice in one turn.
#[test]
fn an_untaken_offer_is_dropped_and_not_repeated() {
    let f = Fixture::new("untaken");
    corpus(&f);
    let items = prompt(ABOUT);
    let mut offers = Offers::new();

    assert!(offers.compose(&f.ws, &f.global, "", &items));
    assert_eq!(offers.tail().len(), 1);

    // The round runs and does something else entirely.
    let settled = offers
        .settle(
            &f.ws,
            "s-test",
            &[call("read", r#"{"path":"src/main.rs"}"#)],
        )
        .cloned()
        .expect("the offer settles");
    assert!(settled.score >= OFFER_FLOOR);

    // The log has one line and it says the offer was not taken.
    let logged = log_lines(&f.ws);
    assert_eq!(logged.len(), 1, "one offer, one line: {logged:?}");
    assert_eq!(logged[0]["taken"], serde_json::json!(false));
    assert_eq!(
        logged[0]["path"],
        serde_json::json!(settled.path.display().to_string())
    );

    // **And the next round composes nothing**: the note is spent for this turn,
    // and the request the model would see carries no offer at all.
    assert!(
        !offers.compose(&f.ws, &f.global, "", &items),
        "a note is offered once per turn"
    );
    assert!(offers.tail().is_empty(), "and then the tail is empty");
}

/// **A take-up is a read of the offered path, and the log records both sides.**
///
/// The signal the design names: *"did a tool call in the next round touch that
/// path?"* — and only a read counts, because only a read puts the note in the
/// conversation.
#[test]
fn a_read_of_the_offered_path_is_the_take_up() {
    let f = Fixture::new("taken");
    let path = corpus(&f);
    let items = prompt(ABOUT);

    // The path as `read` would spell it — workspace-relative, which is what the
    // model sees, while the offer names the absolute one.
    let relative = ".letibot/notes/the-offer-and-the-tail.md";
    let mut offers = Offers::new();
    assert!(offers.compose(&f.ws, &f.global, "", &items));
    let settled = offers
        .settle(
            &f.ws,
            "s-test",
            &[call("read", &format!(r#"{{"path":"{relative}"}}"#))],
        )
        .cloned()
        .expect("the offer settles");
    assert_eq!(settled.path, path, "the note the offer named");
    let logged = log_lines(&f.ws);
    assert_eq!(
        logged[0]["taken"],
        serde_json::json!(true),
        "a `read` of the path is a take-up: {logged:?}"
    );

    // **A `notes` read counts too, by name** — the other door to the same note.
    let mut offers = Offers::new();
    assert!(offers.compose(&f.ws, &f.global, "", &items));
    offers.settle(
        &f.ws,
        "s-test",
        &[call(
            "notes",
            r#"{"action":"read","name":"the-offer-and-the-tail"}"#,
        )],
    );
    let logged = log_lines(&f.ws);
    assert_eq!(logged[1]["taken"], serde_json::json!(true));

    // **And a search that merely mentions the path is not.** `grep` finds the
    // note without putting it in the conversation, and counting it would make the
    // ratio measure interest rather than use.
    let mut offers = Offers::new();
    assert!(offers.compose(&f.ws, &f.global, "", &items));
    offers.settle(
        &f.ws,
        "s-test",
        &[
            call(
                "grep",
                &format!(r#"{{"pattern":"tail","path":"{relative}"}}"#),
            ),
            call("notes", r#"{"action":"search","query":"tail"}"#),
            call("notes", r#"{"action":"add","name":"x","text":"y"}"#),
        ],
    );
    let logged = log_lines(&f.ws);
    assert_eq!(
        logged[2]["taken"],
        serde_json::json!(false),
        "only a read is a take-up: {logged:?}"
    );
}

/// **Silence when unsure** — a subject that does not earn a hint gets none, and
/// so does a note the session has already read.
#[test]
fn nothing_is_said_when_nothing_is_earned() {
    let f = Fixture::new("silent");
    let path = corpus(&f);

    // A subject about something else entirely.
    let mut offers = Offers::new();
    assert!(
        !offers.compose(&f.ws, &f.global, "", &prompt(UNRELATED)),
        "the floor is a floor"
    );
    assert!(offers.tail().is_empty());

    // An empty subject — no prompt at all.
    let mut offers = Offers::new();
    assert!(!offers.compose(&f.ws, &f.global, "", &[]));
    assert!(offers.tail().is_empty());

    // **A note already in the conversation is not offered.** An offer to fetch
    // what the model is already holding is the purest nag there is.
    let mut items = prompt(ABOUT);
    items.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![call(
            "read",
            r#"{"path":".letibot/notes/the-offer-and-the-tail.md"}"#,
        )],
        truncated: false,
    });
    items.push(result("the note's own text, now permanent"));
    let mut offers = Offers::new();
    assert!(
        !offers.compose(&f.ws, &f.global, "", &items),
        "{} was already read",
        path.display()
    );
    assert!(offers.tail().is_empty());

    // **And a note the prompt already carries whole is not offered either** — the
    // case that would otherwise fire every round of every session under the notes
    // budget, where the best-matching note is by construction the one already in
    // front of the model.
    let vocab = letibot_tokencore::Vocab::bytes([], []);
    let section = standing_notes::section(&f.ws, &f.global, &vocab).expect("the notes section");
    assert!(
        section.contains(&format!("### {}", path.display())),
        "the fixture's section carries the note verbatim: {section}"
    );
    let mut offers = Offers::new();
    assert!(
        !offers.compose(&f.ws, &f.global, &section, &prompt(ABOUT)),
        "a note already in the prompt is not offered back"
    );

    // And the same note INDEXED — the shape a file over the notes budget gets — is
    // offered, because its text is exactly what the prompt does not have.
    let indexed = section.replace(
        &format!("### {}", path.display()),
        &format!("### {} — 400 line(s), indexed: over budget", path.display()),
    );
    let mut offers = Offers::new();
    assert!(
        offers.compose(&f.ws, &f.global, &indexed, &prompt(ABOUT)),
        "an indexed note is one a `read` would add something to"
    );
}

/// **The subject is the person's prompt and the turn's tool results**, and it
/// stops at the person's own last row.
#[test]
fn the_subject_is_the_prompt_and_this_turns_tool_results() {
    let mut items = prompt("an older turn that is not what is happening now");
    items.push(TranscriptItem::Assistant {
        text: "…".into(),
        tool_calls: vec![],
        truncated: false,
    });
    items.extend(prompt(ABOUT));
    items.push(result("a tool result from this turn"));
    // A row the harness wrote is not the person's words and does not end the
    // walk — a notice between two results must not hide the prompt under it.
    items.push(TranscriptItem::User {
        speaker: Speaker::Agent,
        parts: vec![UserPart::Text {
            text: "a notice the harness appended".into(),
        }],
    });
    items.push(result("a later tool result"));
    let said = subject(&items);
    assert!(said.contains("this turn"), "{said}");
    assert!(said.contains("a later tool result"), "{said}");
    assert!(
        !said.contains("an older turn"),
        "the walk stops at the person's own last row: {said}"
    );
    assert!(
        !said.contains("a notice the harness appended"),
        "the harness's own row is not the subject: {said}"
    );

    // **And the cut keeps the RECENT text.** A turn that produced more than the
    // cap keeps the end of it, because *what is happening now* is the question.
    let mut long = prompt("the prompt");
    long.push(result(&"old ".repeat(SUBJECT_CHARS)));
    long.push(result("the newest tool result"));
    let long_subject = subject(&long);
    assert!(
        long_subject.len() <= SUBJECT_CHARS + 64,
        "{}",
        long_subject.len()
    );
    assert!(
        long_subject.contains("the newest tool result"),
        "the most recent result survives the cut: {long_subject}"
    );
    assert!(
        !long_subject.contains("the prompt"),
        "and the oldest material is what is dropped: {long_subject}"
    );
}

/// The log, parsed — one JSON object per line, in the order they were written.
fn log_lines(ws: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(ws.join(LOG)).expect("the log exists");
    text.lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}
