//! The cards a head draws for its transcript, as a built-in layer (X22, rank 2 of §4.1).
//!
//! `card_item` is asked once per row, with the row's parts and the typed facts the head holds. It
//! answers one block per part: from the facts when the row carries them (a monitor firing does, and
//! that is the whole point - a `notice_` layer that rewords the sentence must not take the card with
//! it), and otherwise from the readers in `letibot_plugin::card::readers`, which are the SAME code
//! the head falls back to. One source is the whole design: a card cannot be drawn two ways.
//!
//! `card_call` answers the verb a tool row is drawn as, for the calls it knows better than the tool's
//! own name: `notes` is an `edit` or a `write`, which is what `call::drawn_as` says natively. Every
//! other call is answered by nothing and the head's own rule stands.

use letibot_guest_abi as abi;
use letibot_plugin::card::{self, Lines};

#[unsafe(no_mangle)]
pub extern "C" fn card_item(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, the_cards)
}

#[unsafe(no_mangle)]
pub extern "C" fn card_call(ptr: i32, len: i32) -> i32 {
    abi::hook(ptr, len, the_verb)
}

/// One card per part the head named, in its order: a part this layer cannot draw answers a block
/// whose kind is `none`, which the host reads as *the readers' cue* rather than as a refusal.
fn the_cards(bytes: &[u8]) -> Option<String> {
    let input: card::Input = serde_json::from_slice(bytes).ok()?;
    let facts = input.item.facts.as_ref().and_then(fact_cards);
    let blocks: Vec<serde_json::Value> = input
        .parts
        .iter()
        .enumerate()
        .map(|(k, part)| {
            // Facts describe the row, and the row's first part is the delivery they are about; a
            // part after it is another sentence with its own words.
            let from_facts = if k == 0 { facts.clone() } else { None };
            let lines = from_facts
                .or_else(|| card::readers::read(part))
                .and_then(|l| serde_json::to_value(l).ok());
            match lines {
                Some(l) => serde_json::json!({"lines": l}),
                None => serde_json::json!({"none": null}),
            }
        })
        .collect();
    serde_json::to_string(&card::Out { blocks, verb: None }).ok()
}

/// The card a row's typed facts draw, when they are facts this layer has a card for. A row of
/// several notices carries the facts of the delivery it opens with, which is why this is asked of
/// the first part only - and the facts themselves are the SDK's, beside the readers: a card drawn
/// from facts and one drawn from words are one function, so no layer can draw either differently
/// from the head that falls back to it.
fn fact_cards(facts: &serde_json::Value) -> Option<Lines> {
    card::facts::read(facts)
}

/// The verb one call is drawn as, when this layer knows better than the tool's own name.
fn the_verb(bytes: &[u8]) -> Option<String> {
    let input: card::Input = serde_json::from_slice(bytes).ok()?;
    let call = input.item.call?;
    // `notes` hands back a change of its own: rano reads the verb from the name, and the plain name
    // drew a `Ran` row that named the file three times and hid the diff.
    let verb = match call.name.as_str() {
        "notes" => match call.changed.first() {
            Some(first) if first.created => "write",
            Some(_) => "edit",
            None => return None,
        },
        _ => return None,
    };
    serde_json::to_string(&card::Out {
        blocks: Vec::new(),
        verb: Some(verb.to_string()),
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(item: card::Item, parts: &[&str]) -> card::Cards {
        let input = card::Input::new(item, parts.iter().map(|p| p.to_string()).collect());
        let asked = serde_json::to_value(&input).expect("the input serialises");
        let text = the_cards(&serde_json::to_vec(&asked).expect("bytes")).expect("an answer");
        let out: card::Out = serde_json::from_str(&text).expect("an answer document");
        out.cards(parts.len()).expect("the host accepts it")
    }

    fn line(l: &Lines) -> String {
        l.parts.join(" · ")
    }

    /// A rewording the `notice_` door applied leaves the card where it was: the facts name the
    /// watch, and the sentence the model reads is not what the card is drawn from.
    #[test]
    fn a_reworded_notice_keeps_its_card() {
        let facts = serde_json::json!({
            "kind": "monitor",
            "fired": [{
                "name": "ci",
                "said": "[monitor] `ci` fired: the command exited 2.",
                "folded": 0,
                "ending": false
            }],
        });
        let mut item = card::Item::new("s#t1#1", "notice", "the CI watch has news for you");
        item.facts = Some(facts.clone());
        let reworded = ask(item.clone(), &["the CI watch has news for you"]);
        let as_recorded = ask(
            reworded_item(&facts),
            &["[monitor] `ci` fired: the command exited 2."],
        );
        assert_eq!(
            line(reworded.cards[0].as_ref().expect("a card")),
            "monitor · ci the command exited 2 · click for details"
        );
        assert_eq!(
            reworded.cards, as_recorded.cards,
            "the facts and the sentence draw one card"
        );
    }

    fn reworded_item(facts: &serde_json::Value) -> card::Item {
        let mut item = card::Item::new(
            "s#t1#1",
            "notice",
            "[monitor] `ci` fired: the command exited 2.",
        );
        item.facts = Some(facts.clone());
        item
    }

    /// And a row of one kind with no facts is drawn by the readers - byte for byte what the head
    /// draws when no layer answers.
    #[test]
    fn a_row_with_no_facts_draws_as_the_readers_draw_it() {
        let text = "message from your parent session `s-g`: Two new constraints from the operator, \
                    fold them into the plan.\n\n1. REPLICATION";
        let drawn = ask(card::Item::new("s#t1#2", "notice", text), &[text]);
        let readers = card::readers::read(text).expect("a reader knows it");
        assert_eq!(drawn.cards[0].as_ref(), Some(&readers));
    }

    /// One part per block, and a part nothing reads is the readers' cue rather than a refusal.
    #[test]
    fn a_part_no_reader_knows_answers_no_card_for_that_part_alone() {
        let drawn = ask(
            card::Item::new("s#t1#3", "notice", "first\n\nsecond"),
            &[
                "[answer] on \"r\": the operator chose \"X\"",
                "an unread shape, here",
            ],
        );
        assert!(drawn.cards[0].is_some());
        assert_eq!(drawn.cards[1], None);
    }

    /// `notes` is drawn as the change it is; a call this layer has no opinion about answers nothing,
    /// so the head's own rule stands.
    #[test]
    fn notes_is_drawn_as_its_change_and_other_calls_answer_nothing() {
        let mut item = card::Item::new("c1", "call", "");
        item.call = Some(card::Call::new(
            "notes",
            vec![card::Changed {
                path: "a.md".into(),
                created: true,
            }],
        ));
        let asked = serde_json::to_value(card::Input::new(item.clone(), Vec::new()))
            .expect("the input serialises");
        let answer: card::Out = serde_json::from_str(
            &the_verb(&serde_json::to_vec(&asked).expect("bytes")).expect("a verb"),
        )
        .expect("an answer");
        assert_eq!(answer.verb.as_deref(), Some("write"));
        let mut bash = card::Item::new("c2", "call", "");
        bash.call = Some(card::Call::new("bash", Vec::new()));
        let asked =
            serde_json::to_value(card::Input::new(bash, Vec::new())).expect("the input serialises");
        assert!(
            the_verb(&serde_json::to_vec(&asked).expect("bytes")).is_none(),
            "a call with no opinion is the head's own rule"
        );
    }
}
