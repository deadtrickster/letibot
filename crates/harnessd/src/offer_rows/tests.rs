use super::*;
use letibot_sessionlog::hub::Hub;

fn skill(taken: bool) -> skills_offer::Offer {
    skills_offer::Offer {
        name: "rust-skills".into(),
        score: 0.41,
        blended: false,
        taken,
    }
}

/// The operator's row reaches heads and the log, and never the transcript the prompt is rendered
/// from: the model's only copy is the request tail.
#[test]
fn an_offer_is_published_to_heads_and_adds_no_transcript_row() {
    let hub = Hub::new("s");
    let before = hub.snapshot();
    let o = skill(true);
    show_skill(&hub, &o, Some("Run clippy.\nRun fmt."));
    let after = hub.snapshot();
    assert_eq!(after.items, before.items, "a row would enter the prompt");
    let w = after.warnings.last().expect("the offer is a warning");
    assert_eq!(w.code, SKILL_CODE);
    assert!(
        w.detail
            .starts_with("skill offered: rust-skills (0.41) · loaded\n\nRun clippy."),
        "{}",
        w.detail
    );
    let tail = format!("{:?}", o.tail());
    assert!(!tail.contains("Run clippy"), "the tail stays a pointer");
}

/// An untaken offer names no take-up state, a taken one says loaded or read, and the body
/// follows the headline.
#[test]
fn an_untaken_note_offer_names_no_state_and_a_taken_one_says_read() {
    let mut o = notes_offer::Offer {
        path: "/w/notes/build.md".into(),
        score: 0.33,
        blended: false,
        taken: false,
    };
    assert_eq!(
        note_detail(&o, None),
        "note offered: build.md (0.33)",
        "no body, no state"
    );
    o.taken = true;
    let d = note_detail(&o, Some("use make"));
    assert!(
        d.starts_with("note offered: build.md (0.33) · read\n\n/w/notes/build.md\n\nuse make"),
        "{d}"
    );
}

/// A body past the cap is cut on a character boundary and says so.
#[test]
fn a_long_body_is_cut_on_a_boundary() {
    let body = "é".repeat(BODY_MAX);
    let d = skill_detail(&skill(false), Some(&body));
    assert!(d.ends_with("[cut]"));
    assert!(d.len() < BODY_MAX + 100);
}
