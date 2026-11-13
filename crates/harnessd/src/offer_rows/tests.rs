use super::*;

fn skill(taken: bool) -> skills_offer::Offer {
    skills_offer::Offer {
        name: "rust-skills".into(),
        score: 0.41,
        by: skills_offer::By::Subject,
        blended: false,
        taken,
    }
}

/// The row splits into headline and body, and the model's tail stays a pointer.
#[test]
fn an_offer_row_holds_the_headline_and_the_body() {
    let o = skill(true);
    let TranscriptItem::Offer { kind, title, body } = skill_row(&o, Some("Run clippy.\nRun fmt."))
    else {
        panic!("not an offer row");
    };
    assert_eq!(kind, OfferKind::Skill);
    assert_eq!(title, "skill offered: rust-skills (0.41) · loaded");
    assert_eq!(body, "Run clippy.\nRun fmt.");
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
