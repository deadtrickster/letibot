//! The offer, against a fixture corpus and a fixture transcript: no model, no network, no
//! harness. What this module does not do is the subject: it does not put the note in the
//! prompt, it does not put the offer in the log, and it says nothing unless it is sure.

use super::*;
use letibot_transcript::{ToolCall, TranscriptItem, UserPart};

/// A workspace and a box-wide notes dir that remove themselves.
struct Fixture {
    ws: PathBuf,
    global: PathBuf,
    /// The box's two instruction directories, pointed at scratch paths that do not exist, so the
    /// box the suite runs on cannot answer for a fixture's corpus.
    box_dirs: crate::leticode_config::BoxDirs,
    /// The session's substrate: the corpus is read through it, so the fixture has to be rooted
    /// where the notes are, as a session's own backend is.
    backend: letibot_tools::backend::HostBackend,
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
        // The reader spells the workspace as its backend does, which is the resolved path.
        let ws = ws.canonicalize().expect("a real path");
        let box_dirs = crate::leticode_config::BoxDirs {
            config: base.join("box-config"),
            home: base.join("box-home"),
        };
        let backend = letibot_tools::backend::HostBackend::new(&ws).expect("a fixture root");
        Fixture {
            ws,
            global,
            box_dirs,
            backend,
        }
    }

    /// The corpus as a session reads it: through its backend, rooted where it works.
    fn reader(&self) -> crate::standing_notes::Reader<'_> {
        crate::standing_notes::Reader::new(&self.backend, &self.ws)
    }

    /// The one note the fixtures offer: an abstract and headings about the offer design, which is
    /// what the subject below is about.
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

/// The corpus a fixture workspace has: the note under test, and enough others that `idf`
/// has something to be computed over. A corpus of one note makes every term equally rare
/// and no floor discriminates there.
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

/// The offer is composed at the tail, and it is not in the transcript. An assembly can be
/// recomposed and nothing has to be un-said, so the item the request carries is returned by
/// `Offers::tail` and appears in none of the items it was composed from.
#[test]
fn the_offer_is_at_the_tail_and_never_in_the_transcript() {
    let f = Fixture::new("tail");
    let path = corpus(&f);
    let items = prompt(ABOUT);

    let mut offers = Offers::new();
    assert!(
        offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
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

    // And the transcript is untouched: the items handed in are the ones that would be logged
    // which is what makes the offer provisional.
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

/// An offer nobody takes leaves no trace, and the next round says nothing. A discarded
/// offer is dropped from the context entirely, and the same note is not offered twice in
/// one turn.
#[test]
fn an_untaken_offer_is_dropped_and_not_repeated() {
    let f = Fixture::new("untaken");
    corpus(&f);
    let items = prompt(ABOUT);
    let mut offers = Offers::new();

    assert!(offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items));
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

    // And the next round composes nothing: the note is spent for this turn, and the request the
    // model would see carries no offer at all.
    assert!(
        !offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
        "a note is offered once per turn"
    );
    assert!(offers.tail().is_empty(), "and then the tail is empty");
}

/// A take-up is a read of the offered path, and the log records both sides.
///
/// Only a read counts, because only a read puts the note in the conversation.
#[test]
fn a_read_of_the_offered_path_is_the_take_up() {
    let f = Fixture::new("taken");
    let path = corpus(&f);
    let items = prompt(ABOUT);

    // The path as `read` would spell it: workspace-relative, which is what the model sees, while
    // the offer names the absolute one.
    let relative = ".letibot/notes/the-offer-and-the-tail.md";
    let mut offers = Offers::new();
    assert!(offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items));
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

    // A `notes` read counts too, by name: the other door to the same note.
    let mut offers = Offers::new();
    assert!(offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items));
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

    // And a search that merely mentions the path is not: `grep` finds the note without putting it
    // in the conversation, and counting it would measure interest rather than use.
    let mut offers = Offers::new();
    assert!(offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items));
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

/// Silence when unsure: a subject that does not earn a hint gets none, and so does a note the
/// session has already read.
#[test]
fn nothing_is_said_when_nothing_is_earned() {
    let f = Fixture::new("silent");
    let path = corpus(&f);

    // A subject about something else entirely.
    let mut offers = Offers::new();
    assert!(
        !offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &prompt(UNRELATED)),
        "the floor is a floor"
    );
    assert!(offers.tail().is_empty());

    // An empty subject: no prompt at all.
    let mut offers = Offers::new();
    assert!(!offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &[]));
    assert!(offers.tail().is_empty());

    // A note already in the conversation is not offered: an offer to fetch what the model is
    // already holding is the purest nag there is.
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
        !offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
        "{} was already read",
        path.display()
    );
    assert!(offers.tail().is_empty());

    // And a note the prompt already carries whole is not offered either, or it would fire every
    // round under the notes budget, where the best-matching note is the one already in front of
    // the model.
    let vocab = letibot_tokencore::Vocab::bytes([], []);
    let section = standing_notes::section(&f.reader(), &f.global, &f.box_dirs, &vocab)
        .expect("the notes section");
    assert!(
        section.contains(&format!("### {}", path.display())),
        "the fixture's section carries the note verbatim: {section}"
    );
    let mut offers = Offers::new();
    assert!(
        !offers.compose(
            &f.reader(),
            &f.global,
            &f.box_dirs,
            &section,
            &prompt(ABOUT)
        ),
        "a note already in the prompt is not offered back"
    );

    // And the same note INDEXED (the shape a file over the notes budget gets) is offered, because
    // its text is exactly what the prompt does not have.
    let indexed = section.replace(
        &format!("### {}", path.display()),
        &format!("### {} — 400 line(s), indexed: over budget", path.display()),
    );
    let mut offers = Offers::new();
    assert!(
        offers.compose(
            &f.reader(),
            &f.global,
            &f.box_dirs,
            &indexed,
            &prompt(ABOUT)
        ),
        "an indexed note is one a `read` would add something to"
    );
}

/// The subject is the person's prompt and the turn's tool results, and it stops at the person's own
/// last row.
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
    // A row the harness wrote is not the person's words and does not end the walk: a notice
    // between two results must not hide the prompt under it.
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

    // And the cut keeps the RECENT text: a turn that produced more than the cap keeps its end
    // because what is happening now is the question.
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

/// A cut through a multi-byte character must not panic. `String::truncate` asserts that the
/// length is a character boundary, and a subject is full of multi-byte characters.
#[test]
fn a_cut_inside_a_character_does_not_panic() {
    // 7_999 bytes of ASCII, then a 3-byte character placed so that byte `SUBJECT_CHARS` falls in
    // the middle of it: `out.truncate(SUBJECT_CHARS.min(out.len))` with the boundary 8_000 bytes in.
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

/// A note this session was already offered is not offered again: the memory is the
/// session's, not the turn's. The turn's half is the sibling test above.
#[test]
fn a_note_offered_in_an_earlier_turn_is_not_offered_again() {
    let f = Fixture::new("recall");
    let path = corpus(&f);
    let items = prompt(ABOUT);

    // A fresh turn offers it, because nothing in THIS turn has yet.
    let mut fresh = Offers::new();
    assert!(
        fresh.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
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
    // And a line that is not JSON at all: the notes path is best-effort, so one bad line must not
    // cost the session its memory.
    std::fs::write(&log, format!("{line}\nnot json at all\n")).expect("the log");

    let mut said = Offers::new();
    said.recall(&f.ws, "s-mine");
    assert!(
        !said.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
        "an offer already made to this session is not made again"
    );

    // And the memory is the session's, not the box's: another session's line must not silence
    // this one, or a child's offers would mute its parent's.
    let mut other = Offers::new();
    other.recall(&f.ws, "s-other");
    assert!(
        other.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
        "another session's offer is not this session's memory"
    );
}

/// A note this session wrote is not offered back to it. Its author composed every word, so
/// the offer is a fetch of the model's own output, and it wins its round for a structural
/// reason
#[test]
fn a_note_this_session_wrote_is_not_offered_back() {
    let f = Fixture::new("written");
    corpus(&f);

    // Without the write, it is offered.
    let mut fresh = Offers::new();
    assert!(
        fresh.compose(&f.reader(), &f.global, &f.box_dirs, "", &prompt(ABOUT)),
        "the fixture note earns a hint"
    );

    // The session writes it. `notes add` carries a bare stem, which is one of the three spellings
    // `same_note` resolves.
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
        !offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &wrote_it),
        "a note this session wrote is not offered back to it"
    );

    // And it is the note that was written, not "any write at all": writing something else leaves
    // this one offerable.
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
        other.compose(&f.reader(), &f.global, &f.box_dirs, "", &wrote_another),
        "writing a different note does not silence this one"
    );

    // And an `append` counts the same way: the author has read it by writing into it.
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
        !offers2.compose(&f.reader(), &f.global, &f.box_dirs, "", &appended),
        "an append is a write too"
    );
}

/// **A tier that cannot answer leaves the offer exactly as the words scored it**, and the reason
/// is there once for the caller that says it. A box whose endpoint is down keeps its hint, not a
/// silent one and not a crash.
#[test]
fn a_tier_that_cannot_answer_leaves_the_offer_as_the_words_scored_it() {
    let f = Fixture::new("tier-down");
    corpus(&f);
    let data = f.ws.parent().expect("a fixture base").join("data");
    let settings = letibot_embeddings::Settings::from_toml(
        "[embeddings]\nmode = \"http://127.0.0.1:1\"\nmodel = \"m\"\n",
    )
    .expect("settings");
    let embedder = letibot_embeddings::Embedder::open(&settings, &data)
        .expect("open")
        .expect("an embedder");
    let mut with_tier =
        Offers::new().with_tier(Some(letibot_embeddings::Tier::over(embedder.clone())));
    assert!(
        with_tier.compose(&f.reader(), &f.global, &f.box_dirs, "", &prompt(ABOUT)),
        "the note the words earn is still offered"
    );
    let mut without = Offers::new();
    assert!(without.compose(&f.reader(), &f.global, &f.box_dirs, "", &prompt(ABOUT)));
    assert_eq!(
        with_tier.last().map(|o| o.score),
        without.last().map(|o| o.score),
        "and it is the lexical score"
    );
    let said = crate::embeddings::fault_note(Some(&embedder)).expect("the failure is said");
    assert!(said.contains("127.0.0.1:1"), "{said}");
}

/// A paraphrase sharing no word with its note. Words alone score it zero against every note; the second tier's cosine, over words planted as synonyms, carries it over [`OFFER_FLOOR`]. With no tier the offer is unchanged.
#[test]
fn a_paraphrase_with_no_shared_word_is_offered_by_the_second_tier() {
    let f = Fixture::new("paraphrase");
    corpus(&f);
    let path = f.note(
        "vessel-rollout",
        "a vessel sails into production",
        &["Shipping", "The fleet"],
    );
    // Every word of the subject is a word the note does not have: `container` and
    // `deployment` share no term with the title, the abstract or the headings.
    let items = prompt("container deployment");

    let mut lexical = Offers::new();
    assert!(
        !lexical.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
        "the words alone have nothing to go on"
    );

    let tier = letibot_embeddings::testing::planted(
        &f.ws.parent().expect("a fixture base").join("data"),
        &[
            &["vessel", "container"],
            &["rollout", "deployment"],
            &["sails", "ships"],
            &["production", "live"],
        ],
    );
    let mut offers = Offers::new().with_tier(Some(tier));
    assert!(
        offers.compose(&f.reader(), &f.global, &f.box_dirs, "", &items),
        "the meaning earns the hint"
    );
    let offered = offers.tail();
    let text = match &offered[0] {
        TranscriptItem::User { parts, .. } => match &parts[0] {
            UserPart::Text { text } => text.clone(),
            other => panic!("a text part, not {other:?}"),
        },
        other => panic!("a user-side row, not {other:?}"),
    };
    assert!(text.contains(&path.display().to_string()), "{text}");
    assert!(
        text.contains("their meaning"),
        "the line says what the score is: {text}"
    );
}

/// The log, parsed: one JSON object per line, in the order they were written.
fn log_lines(ws: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(ws.join(LOG)).expect("the log exists");
    text.lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}
