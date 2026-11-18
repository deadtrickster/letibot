//! `/note accept` writes the shadow's proposed note with its provenance; `/note decline` writes
//! nothing.

use super::*;
use crate::shadow::{turn_end, verdict::Quoted};
use letibot_transcript::TranscriptItem;

fn a_session(tag: &str) -> (Harness, std::path::PathBuf) {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("note-accept-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = format!("s-note-{tag}");
    at_always_ask(&mut cfg);
    cfg.vocab_gguf = None;
    cfg.notes_home = Some(dir.join("notes"));
    cfg.notes_home_env = true;
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let mut h = Harness::open(&parts, cfg.clone(), Hub::new(&cfg.session_id)).expect("opens");
    let quote = Quoted {
        text: "the gate runs on lab2x1".into(),
        author: "the operator".into(),
        row: Some(0),
    };
    let mut row = turn_end::note_row(&quote, Some("12:08 UTC"), "s-note-shadow");
    if let TranscriptItem::Offer { body, .. } = &mut row {
        body.push_str(&format!(
            "\n\n{}",
            crate::note_accept::hint(h.session.items.len())
        ));
    }
    h.append_offer_rows(&mut vec![row]).expect("the row lands");
    let notes = cfg.project_notes();
    (h, notes)
}

fn written(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn accept_writes_the_note_with_its_provenance() {
    let (mut h, notes) = a_session("accept");
    let at = h.session.items.len() - 1;
    let r = h.answer_note_proposal(true, Some(at));
    assert!(r.ok, "{:?}", r.lines);
    let files = written(&notes);
    assert_eq!(files.len(), 1, "{files:?}");
    let note = &files[0];
    assert!(note.contains("the gate runs on lab2x1"), "{note}");
    assert!(
        note.contains("quoted from the operator"),
        "the author: {note}"
    );
    assert!(note.contains("session s-note-accept, row 0."), "{note}");
    assert!(
        note.contains(&format!("Proposed by shadow s-note-shadow (row {at})")),
        "{note}"
    );
    let again = h.answer_note_proposal(true, None);
    assert!(!again.ok, "a second accept adds no second note");
    assert_eq!(written(&notes).len(), 1);
}

#[test]
fn decline_writes_nothing() {
    let (mut h, notes) = a_session("decline");
    let r = h.answer_note_proposal(false, None);
    assert!(r.ok, "{:?}", r.lines);
    assert!(written(&notes).is_empty());
    let wrong = h.answer_note_proposal(true, Some(99));
    assert!(!wrong.ok, "row 99 is no proposal: {:?}", wrong.lines);
    assert!(written(&notes).is_empty());
}
