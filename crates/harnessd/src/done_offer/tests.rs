use super::*;
use std::path::PathBuf;
use std::sync::Arc;

use letibot_embeddings::{Embedder, Tier};
use letibot_transcript::ToolCall;

use crate::embeddings::fixture;

/// The fixture's words are its directions, so a test is written in them: two groups as far apart as
/// the model can make two sentences.
const SUBJECT: &str = "alpha beta gamma delta";
const BRIEF: &str = "alpha beta gamma";
const ELSEWHERE: &str = "kappa lambda mu nu";

/// A directory that removes itself, for the log.
fn logs(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "harnessd-done-offer-{name}-{}-{}",
        std::process::id(),
        now_secs()
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn a_commit() -> Vec<(PathBuf, String)> {
    subjects_of(&format!("abc1234\t{SUBJECT}\n"))
}

/// A caller's tier over the fixture's embedder: a tier holds one corpus, so a test with two
/// callers takes two of these.
fn caller(embedder: &Arc<Embedder>) -> Offers {
    Offers::new().with_tier(Some(Tier::over(Arc::clone(embedder))))
}

/// The case the caller was filed for: four of fifteen packages found their scope already fixed on
/// main, and a commit subject is where that is written down.
#[test]
fn a_brief_a_commit_already_did_gets_the_offer() {
    let (_m, _d, embedder) = fixture::model("commit");
    let mut offers = caller(&embedder);
    assert!(offers.compose(&a_commit(), BRIEF));
    let made = offers.tail();
    assert_eq!(made.len(), 1, "{made:?}");
    let TranscriptItem::User { parts, speaker } = &made[0] else {
        panic!("an offer is a user item: {made:?}")
    };
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness's own line, and it says so"
    );
    let UserPart::Text { text } = &parts[0] else {
        panic!("text")
    };
    assert!(
        text.contains("abc1234"),
        "the sha, so it can be checked: {text}"
    );
    assert!(
        text.contains("git show"),
        "the command that settles it: {text}"
    );
    let settled = offers.settle(&logs("commit"), "s-1", &[]).expect("settled");
    assert!(settled.commit);
    assert_eq!(settled.label, "abc1234");
}

/// And a brief that nothing in the tree has done says nothing: the line is worth reading or it is
/// not written.
#[test]
fn an_unrelated_brief_is_said_nothing() {
    let (_m, _d, embedder) = fixture::model("unrelated");
    let records = a_commit();
    let mut offers = caller(&embedder);
    assert!(!offers.compose(&records, ELSEWHERE));
    assert!(offers.tail().is_empty());
    // The record IS the best match and is still under the floor: silence is the floor's work.
    let scored_corpus = Corpus::of_texts(&records);
    let tier = Tier::over(Arc::clone(&embedder));
    scored_corpus.fill(Some(&tier)).expect("a fill");
    let (_, scored) = scored_corpus
        .best_with(ELSEWHERE, Some(&tier))
        .expect("a best");
    assert!(scored < OFFER_FLOOR, "{scored} of 1.00 is not silence");
    // An empty brief, and a tree with no records at all.
    assert!(!offers.compose(&records, "  \n "));
    assert!(!offers.compose(&[], BRIEF));
}

/// Mode off changes nothing: no tier, no offer, an unchanged request.
#[test]
fn mode_off_composes_nothing() {
    let mut offers = Offers::new();
    assert!(!offers.compose(&a_commit(), BRIEF));
    assert!(offers.tail().is_empty());
}

/// One record is offered once per session, and the log is the memory: a restart does not say it
/// again, and another session is its own memory.
#[test]
fn a_record_already_offered_is_not_offered_again() {
    let dir = logs("once");
    let (_m, _d, embedder) = fixture::model("once");
    let records = a_commit();
    let mut offers = caller(&embedder);
    assert!(offers.compose(&records, BRIEF));
    offers.settle(&dir, "s-1", &[]);
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(
        logged.contains("\"record\":\"abc1234\"")
            && logged.contains("\"commit\":true")
            && logged.contains("\"taken\":false"),
        "{logged}"
    );
    let mut after = caller(&embedder);
    after.recall(&dir, "s-1");
    assert!(!after.compose(&records, BRIEF));
    let mut other = caller(&embedder);
    other.recall(&dir, "s-2");
    assert!(other.compose(&records, BRIEF));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A call that names the record is the take-up: checking is the whole of what the line asks for,
/// and a call about something else is not.
#[test]
fn a_call_that_names_the_record_is_the_take_up() {
    let dir = logs("taken");
    let (_m, _d, embedder) = fixture::model("taken");
    let mut offers = caller(&embedder);
    assert!(offers.compose(&a_commit(), BRIEF));
    let checking = ToolCall {
        id: "c".into(),
        name: "bash".into(),
        arguments: "{\"command\":\"git show abc1234\"}".into(),
    };
    assert!(offers.settle(&dir, "s-1", &[checking]).is_some());
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(logged.contains("\"taken\":true"), "{logged}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A log line is a sha and a subject.
#[test]
fn the_log_is_read_by_its_shape() {
    let log = "abc1234\ta subject here\n\ndeadbee\tanother subject\nnot a record\n";
    let records = subjects_of(log);
    assert_eq!(
        records,
        vec![
            (PathBuf::from("abc1234"), "a subject here".to_string()),
            (PathBuf::from("deadbee"), "another subject".to_string()),
        ]
    );
}

/// A workspace that is not a repository has no commits: an empty corpus, not a failure.
#[test]
fn a_tree_that_is_not_a_repository_has_no_commits() {
    let dir = logs("plain");
    std::fs::create_dir_all(&dir).expect("scratch");
    assert!(records(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
