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

/// **A cut through a multi-byte character must not panic.**
///
/// Measured 2026-10-10, on the box, and it is the worst failure this module has had:
/// `String::truncate` asserts that the length it is handed is a boundary of the
/// string, and the subject is the operator's own words plus the turn's tool results —
/// full of `—`, `⚠`, `→`, box-drawing out of a `capture-pane` and Cyrillic. A subject
/// whose cut fell inside one of those characters raised
/// `assertion failed: self.is_char_boundary(new_len)` on `thread 'main'`, so the
/// daemon died from the offer path — the path whose own contract, above in this
/// module, is that *a hint that fails is still a hint*. Three panics in one evening,
/// one of them a subagent's thread.
///
/// Every fixture in this file is ASCII, which is the whole reason the suite stayed
/// green while the daemon was dying.
#[test]
fn a_cut_inside_a_character_does_not_panic() {
    // 7_999 bytes of ASCII, then a 3-byte character placed so that byte
    // `SUBJECT_CHARS` falls in the middle of it. This is the shape that killed it:
    // `out.truncate(SUBJECT_CHARS.min(out.len()))` with a boundary 8_000 bytes in.
    let text = format!("{}☃ and on", "a".repeat(SUBJECT_CHARS - 1));
    let said = subject(&[result(&text)]);
    assert!(
        said.is_char_boundary(said.len()),
        "the cut is not on a character boundary"
    );
    assert!(
        said.len() <= SUBJECT_CHARS,
        "the cap was exceeded: {} bytes",
        said.len()
    );
    assert_eq!(
        said.len(),
        SUBJECT_CHARS - 1,
        "the cut moves back to the nearest boundary at or below the cap, and no further"
    );
    assert!(
        text.starts_with(&said),
        "the cut kept more than it was given"
    );
}

/// **A note this session was already offered is not offered again — the memory is
/// the session's, not the turn's.**
///
/// The sibling test above is the turn's half of this. Measured 2026-10-10 on the
/// box, that half was the only one that existed: `Offers` was built fresh each turn,
/// so *"once per note"* meant once per TURN, and one note was offered 22 times across
/// a session's turns — 120 offers in the log, 19 taken, every repeat a row in the
/// operator's transcript. Their words for it: *"the notes keep pilin up.
/// unacceptable"*.
#[test]
fn a_note_offered_in_an_earlier_turn_is_not_offered_again() {
    let f = Fixture::new("recall");
    let path = corpus(&f);
    let items = prompt(ABOUT);

    // A fresh turn offers it, because nothing in THIS turn has yet.
    let mut fresh = Offers::new();
    assert!(
        fresh.compose(&f.ws, &f.global, "", &items),
        "the fixture note earns a hint"
    );

    // The session's earlier offer, written exactly where `settle` writes it.
    let log = f.ws.join(LOG);
    let line = serde_json::json!({
        "at": 1,
        "session": "s-mine",
        "path": path.display().to_string(),
        "score": 0.2,
        "taken": false,
    })
    .to_string();
    // And a line that is not JSON at all: the notes path is best-effort, so a
    // corpus of one bad line must not cost the session its memory.
    std::fs::write(&log, format!("{line}\nnot json at all\n")).expect("the log");

    let mut said = Offers::new();
    said.recall(&f.ws, "s-mine");
    assert!(
        !said.compose(&f.ws, &f.global, "", &items),
        "an offer already made to this session is not made again"
    );

    // **And the memory is the session's, not the box's**: another session's line
    // must not silence this one, or a child's offers would mute its parent's.
    let mut other = Offers::new();
    other.recall(&f.ws, "s-other");
    assert!(
        other.compose(&f.ws, &f.global, "", &items),
        "another session's offer is not this session's memory"
    );
}

/// **A note this session wrote is not offered back to it.**
///
/// Its author composed every word, so the offer is a fetch of the model's own output — and it
/// wins its round for a structural reason: a note about what you are doing scores highest
/// against the subject you are writing about. MEASURED 2026-10-10, one evening: four of fourteen
/// offers were notes the same session had just written, each at the top of its round.
#[test]
fn a_note_this_session_wrote_is_not_offered_back() {
    let f = Fixture::new("written");
    corpus(&f);

    // Without the write, it is offered.
    let mut fresh = Offers::new();
    assert!(
        fresh.compose(&f.ws, &f.global, "", &prompt(ABOUT)),
        "the fixture note earns a hint"
    );

    // The session writes it. `notes add` carries a bare stem, which is one of the three
    // spellings `same_note` resolves.
    let mut wrote_it = prompt(ABOUT);
    wrote_it.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![call(
            "notes",
            r#"{"action":"add","name":"the-offer-and-the-tail"}"#,
        )],
        truncated: false,
    });
    let mut offers = Offers::new();
    assert!(
        !offers.compose(&f.ws, &f.global, "", &wrote_it),
        "a note this session wrote is not offered back to it"
    );

    // **And it is the note that was written, not 'any write at all'.** Writing something else
    // leaves this one offerable.
    let mut wrote_another = prompt(ABOUT);
    wrote_another.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![call(
            "notes",
            r#"{"action":"add","name":"some-other-note"}"#,
        )],
        truncated: false,
    });
    let mut other = Offers::new();
    assert!(
        other.compose(&f.ws, &f.global, "", &wrote_another),
        "writing a different note does not silence this one"
    );

    // And an `append` to it counts the same way — the author has read it by writing into it.
    let mut appended = prompt(ABOUT);
    appended.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![call(
            "notes",
            r#"{"action":"append","name":"the-offer-and-the-tail"}"#,
        )],
        truncated: false,
    });
    let mut offers2 = Offers::new();
    assert!(
        !offers2.compose(&f.ws, &f.global, "", &appended),
        "an append is a write too"
    );
}

/// The log, parsed — one JSON object per line, in the order they were written.
fn log_lines(ws: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(ws.join(LOG)).expect("the log exists");
    text.lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}
