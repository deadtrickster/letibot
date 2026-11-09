//! The cards a row's typed facts draw, with no text read at all: what survives a `notice_` layer
//! rewording the sentence the model reads (X22).
//!
//! One source, like the readers beside it: the built-in `cards` layer answers from these, and the
//! head falls back to them when no layer answers - a card drawn from a row's facts and one drawn
//! from its words are the same function, so which of the two drew a line is invisible.

use serde_json::Value;

use super::Lines;
use super::readers;

/// The card a row's facts draw, or `None` for facts this build has no card for.
///
/// A monitor firing is the one kind a row carries today (`TranscriptItem::Notice`'s `facts`); a kind
/// added here is a kind the daemon's own row must start carrying, which is why the list is short:
/// most kinds' facts never leave the daemon, which joins its sentences into one row.
pub fn read(facts: &Value) -> Option<Lines> {
    match facts.get("kind")?.as_str()? {
        "monitor" => {
            let bullets: Vec<(String, String)> = facts
                .get("fired")?
                .as_array()?
                .iter()
                .filter_map(|f| {
                    let name = f.get("name")?.as_str()?.to_string();
                    let said = f.get("said")?.as_str()?;
                    Some((name, readers::firing_said(said)))
                })
                .collect();
            readers::monitor_card(&bullets)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(l: &Lines) -> String {
        l.parts.join(" · ")
    }

    /// A firing's facts drawn as the same card its sentence draws - the two spellings of one
    /// notice, which is what makes a rewording harmless.
    #[test]
    fn a_firings_facts_and_its_sentence_draw_one_card() {
        let facts = serde_json::json!({
            "kind": "monitor",
            "fired": [{
                "name": "ci",
                "said": "[monitor] `ci` fired: the command exited 2.",
                "folded": 0,
                "ending": false,
            }],
        });
        let from_facts = read(&facts).expect("monitor facts are a card");
        let from_text = readers::read("[monitor] `ci` fired: the command exited 2.")
            .expect("the sentence is a card");
        assert_eq!(line(&from_facts), line(&from_text));
        assert_eq!(
            line(&from_facts),
            "monitor · ci the command exited 2 · click for details"
        );
    }

    /// Facts this build has no card for are the readers' cue, and facts that are not facts at all
    /// are `None` rather than an empty card.
    #[test]
    fn facts_with_no_card_answer_nothing() {
        assert!(read(&serde_json::json!({"kind": "census", "rows": 3})).is_none());
        assert!(read(&serde_json::json!("not facts")).is_none());
        assert!(read(&serde_json::json!({"kind": "monitor"})).is_none());
    }
}
