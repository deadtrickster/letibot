//! Standing notes against a real harness: the two moments the head of the
//! prompt may move, and the one moment it may move mid-session.
//!
//! The assembly itself — the sources, the order, the budget, the digest — is
//! asserted as unit tests on `standing_notes`, which needs no harness. What
//! needs one is the part the operator actually asked for:
//!
//! 1. **The re-read at a base rebuild.** `reseat_target` re-reads the files,
//!    so an `AGENTS.md` edited between two compactions lands in the new
//!    base's message 0 — and only the notes move, with everything the
//!    composed prompt carries beside them carried through byte for byte,
//!    including a fabric block appended after the section the way
//!    `Sessions::with_fabric` leaves it.
//! 2. **The per-turn delivery on a provider session.** `pending_standing_notes_update`
//!    produces the `System { origin: Update }` item for a change, nothing for
//!    no change, a sentence for a removal, and — after a fork adopted fresher
//!    notes — nothing again, because the tracker follows what the new message 0
//!    carries rather than remembering beside it.
//!
//! It needs the vocabulary GGUF and not the model server: nothing here runs a
//! turn. Same gate as `compact.rs`, for the same reason.

use letibot_harnessd::config::Config;
use letibot_harnessd::standing_notes;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_transcript::TranscriptItem;

/// A directory that removes itself, so the fixtures here never compete for a
/// name under `/tmp` (the same shape `compact.rs` rolls for itself).
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p =
            std::env::temp_dir().join(format!("harnessd-standing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session over a workspace with a nonce in its `AGENTS.md`, composed the
/// way `Sessions` composes one: the prompt is built before the harness opens,
/// notes included.
fn opened(workspace: &std::path::Path, nonce: &str) -> Harness {
    let dir = TempDir::new("store");
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(dir.path().join("sessions.db"));
    cfg.session_id = format!("standing-{nonce}");
    cfg.vocab_gguf = std::env::var("LETIBOT_VOCAB_GGUF")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(letibot_tokencore::apparatus::present_gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    std::fs::write(workspace.join("AGENTS.md"), format!("note-{nonce}-one\n"))
        .expect("fixture AGENTS.md");
    cfg.compose_system_with_notes(&parts.vocab);
    let hub = Hub::new(&cfg.session_id);
    Harness::open(&parts, cfg, hub).expect("the session must open")
}

/// **An `AGENTS.md` edited between the compose and the rebuild shows up in the
/// new base.**
///
/// This is the operator's ask verbatim — *"make sure AGENTS.md reread after
/// each compaction"* — driven at the seam every fork computes its next prefix
/// through, without a model server: `reseat_target` is the offline-drivable
/// half of a compaction by its own doc.
#[test]
fn an_edited_agents_md_lands_in_the_next_base() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-rebuild");
    let mut h = opened(ws.path(), "rebuild");

    // Nothing changed on disk and the seated tools are the same, so the next
    // base is the prompt being spoken: no fork, no new prefix row.
    assert!(
        h.reseat_target().expect("unchanged").is_none(),
        "an unchanged file must not fork the conversation"
    );
    let before = h.config().system.clone();

    // The edit. A nonce, so the two generations of the file cannot be confused
    // by shared words.
    std::fs::write(ws.path().join("AGENTS.md"), "note-rebuild-two\n").expect("edit");

    let Some((next, _id)) = h.reseat_target().expect("changed") else {
        panic!("the edited AGENTS.md did not reach the next base");
    };
    assert!(
        next.system.contains("note-rebuild-two"),
        "the new generation is in message 0"
    );
    assert!(
        !next.system.contains("note-rebuild-one"),
        "the old generation is gone"
    );
    // **And nothing else moved.** Strip the section from both and the prompts
    // are byte-identical: the sections, `system_extra`, the budget arithmetic —
    // the whole frozen-input rule `compose_system` set keeps its side of the
    // bargain and only the notes moved.
    assert_eq!(
        standing_notes::replace(&next.system, None),
        standing_notes::replace(&before, None),
        "everything outside the notes section is carried through unchanged"
    );

    // **A fabric block appended after the section survives the swap** — the
    // shape `Sessions::with_fabric` leaves the prompt in, and the reason the
    // swap is by marker rather than a recompose.
    h.config_mut().system.push_str("\n\nfabric block goes last");
    std::fs::write(ws.path().join("AGENTS.md"), "note-rebuild-three\n").expect("edit");
    let Some((next, _)) = h.reseat_target().expect("changed again") else {
        panic!("the second edit did not reach the next base");
    };
    assert!(
        next.system.contains("note-rebuild-three"),
        "{}",
        next.system
    );
    assert!(
        next.system.ends_with("fabric block goes last"),
        "the fabric block, appended after the section, is untouched: {}",
        next.system
    );
}

/// **The per-turn delivery: an item for a change, nothing for none, a sentence
/// for a removal, and nothing again after a fork adopted the change.**
///
/// The provider guard itself (`submit_item`'s one-line rule) is not driven
/// here — a provider backend offline is an environment question, and the rule
/// is a line beside `compact_inner`'s own `remote` predicate. What is driven
/// is the mechanism the guard admits.
#[test]
fn the_per_turn_check_produces_an_item_only_when_the_files_changed() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-turn");
    let mut h = opened(ws.path(), "turn");

    // Message 0 carries the notes, so the first check has nothing to say.
    assert!(
        h.pending_standing_notes_update().is_none(),
        "unchanged files produce no update"
    );

    // The edit: one item, saying it replaces the section and carrying the new
    // generation. The SHAPE is the dialect's choice — Qwen wraps a change in a
    // `<system-update>` envelope because its template only honours a system
    // prompt at position 0, and an `InHistory` dialect appends a `System` row
    // — so what is asserted is the content and that it is a delivery the
    // dialect renders, not one variant of the two.
    std::fs::write(ws.path().join("AGENTS.md"), "note-turn-two\n").expect("edit");
    let item = h
        .pending_standing_notes_update()
        .expect("the change is delivered");
    let text = item_text(&item);
    assert!(
        text.contains("note-turn-two"),
        "the new generation is carried: {text}"
    );
    assert!(
        text.contains("replaces the standing-notes section"),
        "the item says what it does to the prompt: {text}"
    );
    assert!(
        text.contains("[standing-notes-begin]"),
        "the item carries the same envelope the prompt's section does: {text}"
    );
    assert!(
        text.contains("system-update") || matches!(&item, TranscriptItem::System { .. }),
        "the delivery rides in the dialect's system-update form: {item:?}"
    );
    // And once, not once per turn: the tracker moved with the delivery.
    assert!(
        h.pending_standing_notes_update().is_none(),
        "the same files produce no second item"
    );

    // A fork that adopts fresher notes moves the tracker with it, so the turn
    // after a compaction does not re-deliver what message 0 already carries.
    std::fs::write(ws.path().join("AGENTS.md"), "note-turn-three\n").expect("edit");
    let (next, id) = h
        .reseat_target()
        .expect("changed")
        .expect("the edit forks a new base");
    h.adopt_reseat(Some((next, id)));
    assert!(
        h.pending_standing_notes_update().is_none(),
        "the fork's message 0 already carries the newest notes"
    );

    // **A removal is announced as itself**, not left to be inferred from a
    // section that silently stopped applying.
    std::fs::remove_file(ws.path().join("AGENTS.md")).expect("remove");
    let item = h
        .pending_standing_notes_update()
        .expect("a removal is a change");
    let text = item_text(&item);
    assert!(
        text.contains("removed from disk") && text.contains("no longer applies"),
        "the removal says what happened: {text}"
    );
}

/// The prose of a delivery item, whichever shape the dialect chose for it.
fn item_text(item: &TranscriptItem) -> String {
    match item {
        TranscriptItem::System { text, .. } => text.clone(),
        TranscriptItem::User { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                letibot_transcript::UserPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("a delivery is prose-carrying, not {other:?}"),
    }
}
