//! K6: a row's card comes from the typed facts it was delivered with, through the `card_` hook
//! (`docs/plugin-extension-points-wide.md` X22). The built-in `cards` layer is compiled from the
//! same readers the head falls back to, so the three things these tests pin are: a reworded notice
//! keeps its card, a row is asked about once, and the layer draws what the readers drew.

use super::*;
use letibot_plugins::Stack;
use letibot_transcript::NoticeKind;
use std::path::PathBuf;
use std::sync::Arc;

/// A scratch project, so the stack has somewhere to look for a project's layers that is not the
/// tree this test runs in.
fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "letibot-cards-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("a scratch project");
    dir
}

/// The head's host over the BINARY's own layers and nothing else - the real engine, the real
/// modules, no fake answer.
fn builtin_host(tag: &str) -> crate::app::sessions_pane::Host {
    let dir = project(tag);
    let mut stack = Stack::discover(
        None,
        Some(&dir),
        None,
        Arc::new(crate::app::sessions_pane::RanoLoader),
    );
    let failed = letibot_builtins::install(&mut stack);
    assert!(failed.is_empty(), "every built-in loads: {failed:?}");
    crate::app::sessions_pane::Host::with_stack(stack, dir)
}

/// One delivery row: a `Notice` with the sentence and the facts the daemon composed it from.
fn notice_row(id: &str, text: &str, facts: Option<serde_json::Value>) -> SnapshotItem {
    SnapshotItem {
        citations: Vec::new(),
        item_id: id.into(),
        kind: "row".into(),
        ledger_head: String::new(),
        ts: 0,
        item: Some(TranscriptItem::Notice {
            kind: NoticeKind::Monitor,
            text: text.into(),
            facts,
        }),
    }
}

/// One monitor firing's facts, as `deliveries::monitor_facts` builds them.
fn firing(name: &str, why: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "monitor",
        "fired": [{
            "name": name,
            "said": format!("[monitor] `{name}` fired: {why}."),
            "folded": 0,
            "ending": false,
        }],
    })
}

/// A `notice_` layer may reword the sentence a model reads; the card is drawn from the facts the
/// row carries, so it survives the rewording - which is the whole of what this hook is for.
#[test]
fn a_reworded_notice_keeps_its_card() {
    let host = builtin_host("reworded");
    let facts = firing("ci", "the command exited 2");
    // The sentence as the harness composed it, and the same facts under a layer's own words: one
    // card, whatever the row says.
    let as_said = notice_row(
        "s#t1#1",
        "[monitor] `ci` fired: the command exited 2.",
        Some(facts.clone()),
    );
    let reworded = notice_row(
        "s#t1#2",
        "your CI watch has something to tell you",
        Some(facts),
    );
    let one = Cards::row(Some(&host), &as_said);
    let other = Cards::row(Some(&host), &reworded);
    let drawn = one.lines(&["[monitor] `ci` fired: the command exited 2."]);
    let reworded_drawn = other.lines(&["your CI watch has something to tell you"]);
    assert_eq!(
        drawn[0].line,
        "monitor · ci the command exited 2 · click for details"
    );
    assert_eq!(
        reworded_drawn[0].line, drawn[0].line,
        "the reworded row drew from its facts, not its words"
    );
    // And the readers alone cannot do this: the sentence has no shape they know, so the card would
    // have been the generic line. That is the defect the facts close.
    let readers = Cards::readers_only().lines(&["your CI watch has something to tell you"]);
    assert_eq!(
        readers[0].line,
        "notice · your CI watch has something to tell you · click for details"
    );
    // The same row drawn with no stack at all: the facts are the head's own reading too, so a head
    // that never built a plugin stack still keeps the card through the rewording.
    assert_eq!(
        Cards::row(None, &reworded).lines(&["your CI watch has something to tell you"])[0].line,
        "monitor · ci the command exited 2 · click for details"
    );
}

/// A row is asked about ONCE: the answer is kept by its id and what the layer was asked, so a frame
/// redrawn a hundred times is one call and not a hundred.
#[test]
fn a_card_is_asked_once_per_item() {
    let host = builtin_host("once");
    let text = "[answer] on \"GAP (latency)\": you asked \"Postpone?\" — the operator chose \
                \"Postpone it\"";
    let row = notice_row("s#t1#3", text, None);
    let first = Cards::row(Some(&host), &row).lines(&[text]);
    assert_eq!(host.cards_asked(), 1, "the first draw asks the layer");
    // Three more frames, and a second `Cards` over the same row: still one ask.
    for _ in 0..3 {
        let again = Cards::row(Some(&host), &row).lines(&[text]);
        assert_eq!(again[0].line, first[0].line);
    }
    assert_eq!(
        host.cards_asked(),
        1,
        "a card is asked once per item, not once per frame"
    );
}

/// A row is not frozen at the words it was read with. The same id with other words is another
/// question - a card kept under the id alone would draw the old sentence for the rest of the session.
#[test]
fn an_edited_row_asks_for_its_card_again() {
    let host = builtin_host("edited");
    let before = "[monitor] `ci` fired: the command exited 2.";
    let row = notice_row("s#t1#7", before, Some(firing("ci", "the command exited 2")));
    let first = Cards::row(Some(&host), &row).lines(&[before]);
    assert_eq!(host.cards_asked(), 1, "the first draw asks the layer");
    assert_eq!(
        Cards::row(Some(&host), &row).lines(&[before])[0].line,
        first[0].line
    );
    assert_eq!(host.cards_asked(), 1, "and redrawing it does not ask again");
    // The same row, edited: the same id, other words, and the facts that go with them.
    let after = "[monitor] `ci` fired: the command exited 7.";
    let edited = notice_row("s#t1#7", after, Some(firing("ci", "the command exited 7")));
    let redrawn = Cards::row(Some(&host), &edited).lines(&[after]);
    assert_eq!(
        host.cards_asked(),
        2,
        "an edited row is a question of its own"
    );
    assert_eq!(
        redrawn[0].line,
        "monitor · ci the command exited 7 · click for details"
    );
}

/// The cache is bounded, and what it drops is the answer nobody has asked for longest - not the
/// whole thing at a cliff, which would re-ask for every row a long session still draws.
#[test]
fn the_card_cache_evicts_the_least_recently_used() {
    let mut cache = CardCache::default();
    for k in 0..MAX_CARDS {
        cache.insert(format!("row {k}"), Some(serde_json::json!(k)));
    }
    assert_eq!(cache.len(), MAX_CARDS, "the cap is the cap");
    // Ask for the oldest again: it is now the most recently used, so the NEXT insert is the one
    // that takes it out of the way.
    assert_eq!(cache.get("row 0"), Some(Some(serde_json::json!(0))));
    cache.insert("one more".to_string(), None);
    assert_eq!(cache.len(), MAX_CARDS, "still bounded");
    assert!(cache.get("row 0").is_some(), "the row just asked for stays");
    assert!(cache.get("row 1").is_none(), "the least recently used went");
    assert_eq!(
        cache.get("one more"),
        Some(None),
        "and a row the layer drew nothing for is an answer too"
    );
}

/// The built-in `cards` layer draws what the readers draw, on the readers' own fixtures: the layer
/// is not a second drawing of a notice but the same one, compiled into a module. A fixture no reader
/// knows is the layer's cue to say nothing - the head's own generic line is still drawn - and the
/// test says so rather than pretending the layer answered it.
#[test]
fn the_builtin_cards_layer_draws_what_the_readers_drew() {
    let host = builtin_host("fixtures");
    let mut drawn_by_the_layer = 0;
    for (k, (text, want)) in letibot_plugin::card::readers::FIXTURES.iter().enumerate() {
        let row = notice_row(&format!("s#t1#{}", 100 + k), text, None);
        let cards = Cards::row(Some(&host), &row);
        // The layer's own answer - not the fallback this head would draw if the layer said nothing.
        let answered = cards
            .answered(&[text])
            .unwrap_or_else(|| panic!("the layer refused the ask for {text}"));
        match &answered[0] {
            Some(lines) => {
                assert_eq!(
                    lines.parts.join(" · "),
                    *want,
                    "the layer's card, for {text}"
                );
                drawn_by_the_layer += 1;
            }
            None => assert!(
                letibot_plugin::card::readers::read(text).is_none(),
                "the layer drew nothing for a shape the readers know: {text}"
            ),
        }
        assert_eq!(
            cards.lines(&[text])[0].line,
            *want,
            "and the row drawn through it, for {text}"
        );
        assert_eq!(
            crate::ui::transcript::notice_card::session_notice(text).line,
            *want,
            "and the readers' own card, which is the fallback, for {text}"
        );
    }
    assert!(
        drawn_by_the_layer > 1,
        "the layer answered {drawn_by_the_layer} of {} fixtures",
        letibot_plugin::card::readers::FIXTURES.len()
    );
}
