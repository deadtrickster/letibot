//! The skills pane: the box's skills, one row each with where each was found, and a way to hand one
//! to the session. Two halves, like the standing pane's fixtures: a `Skills` frame built by hand,
//! and the head's own doors, which are what this file asserts.

use super::*;
use crate::app::Key;
use letibot_sessionlog::protocol::SkillRow;

fn rows() -> Vec<SkillRow> {
    vec![
        SkillRow {
            name: "release".into(),
            source: "this project".into(),
            description: "cut a release".into(),
            path: None,
        },
        SkillRow {
            name: "review-a-branch".into(),
            source: "the operator's".into(),
            description: "judge a branch".into(),
            path: None,
        },
        SkillRow {
            name: "plugin-pane".into(),
            source: "this build".into(),
            description: "write a pane plugin".into(),
            path: None,
        },
    ]
}

fn frame(skills: Vec<SkillRow>) -> ServerFrame {
    ServerFrame::Skills {
        session_id: "s1".into(),
        skills,
    }
}

/// `/skills` opens the pane and asks the daemon, and the pane draws every row with the source
/// it came from - the one fact a reader weighs a skill by.
#[test]
fn slash_skills_opens_the_pane_and_the_rows_carry_their_sources() {
    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(a.command("skills"), Some(Action::ListSkills));
    a.apply(frame(rows()));
    let drawn = a.screen(120, 40).join("\n");
    for (name, source) in [
        ("release", "this project"),
        ("review-a-branch", "the operator's"),
        ("plugin-pane", "this build"),
    ] {
        assert!(drawn.contains(name), "no `{name}`: {drawn}");
        assert!(
            drawn.contains(&format!("[{source}]")),
            "no source for `{name}`: {drawn}"
        );
    }
}

/// `/skills NAME` hands that skill over, and the rest of the line goes on as the operator's
/// prompt in the same turn - which is why the action carries it rather than a second send.
#[test]
fn slash_skills_names_one_and_carries_the_rest_of_the_line() {
    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(
        a.command("skills release cut 0.5.2"),
        Some(Action::LoadSkill {
            name: "release".into(),
            then: "cut 0.5.2".into(),
        })
    );
    // A bare name is the same act with nothing after it.
    assert_eq!(
        a.command("skills release"),
        Some(Action::LoadSkill {
            name: "release".into(),
            then: String::new(),
        })
    );
}

/// Enter opens the skill's file in the editor - the row under the cursor - and sends nothing
/// to the session.
#[test]
fn enter_on_a_skill_row_opens_its_file_in_the_editor() {
    let d = std::env::temp_dir().join(format!("letibot-skills-open-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("SKILL.md");
    std::fs::write(&file, "---\nname: review-a-branch\n---\njudge it\n").unwrap();
    let mut rs = rows();
    rs[1].path = Some(file.display().to_string());
    let mut a = app();
    a.session_id = "s1".into();
    a.open_surface(Surface::Skills);
    a.apply(frame(rs));
    a.screen(120, 24);
    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), None, "nothing goes to the session");
    assert!(a.editor_focused(), "enter hands the keyboard to the editor");
    let _ = std::fs::remove_dir_all(&d);
}

/// A skill with no file (built in) opens nothing and says so.
#[test]
fn enter_on_a_skill_without_a_file_says_so() {
    let mut a = app();
    a.session_id = "s1".into();
    a.open_surface(Surface::Skills);
    a.apply(frame(rows()));
    a.screen(120, 24);
    assert_eq!(a.key(Key::Enter), None);
    assert!(!a.editor_focused());
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("not a file"))
    );
}

/// `s` hands the row under the cursor over, by name - the deliberate act.
#[test]
fn s_on_a_skill_row_hands_that_skill_over() {
    let mut a = app();
    a.session_id = "s1".into();
    a.open_surface(Surface::Skills);
    a.apply(frame(rows()));
    a.screen(120, 40);
    a.key(Key::Down);
    let act = a.key(Key::Char('s'));
    assert_eq!(
        act,
        Some(Action::LoadSkill {
            name: "review-a-branch".into(),
            then: String::new(),
        })
    );
}

/// The notice says what is HAPPENING, and the daemon's answer is the outcome: it said `handed to
/// the session` before anything had been asked, over a skill the daemon may still refuse.
#[test]
fn the_notice_says_handing_until_the_daemon_answers() {
    let mut a = app();
    a.session_id = "s1".into();
    a.open_surface(Surface::Skills);
    a.apply(frame(rows()));
    a.screen(120, 40);
    assert!(matches!(
        a.key(Key::Char('s')),
        Some(Action::LoadSkill { .. })
    ));
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("handing `release` to the session"), "{said}");
    assert!(
        !said.contains("handed"),
        "nothing has been handed over yet: {said}"
    );
    // The acceptance's own words replace the act's.
    a.apply(ServerFrame::Accepted {
        client_request_id: "r1".into(),
        seq: 1,
        note: "`release` queued — its body is handed to the model in the next turn".into(),
    });
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("`release` queued"), "{said}");
    // And a refusal is the refusal, not a second handing.
    assert!(matches!(
        a.key(Key::Char('s')),
        Some(Action::LoadSkill { .. })
    ));
    a.apply(ServerFrame::Rejected {
        client_request_id: "r2".into(),
        reason: "no skill is called `release` here. Nothing was loaded.".into(),
        expected_seq: 1,
        actual_seq: 1,
    });
    let said = a.notice.clone().unwrap_or_default();
    assert!(said.contains("no skill is called `release`"), "{said}");
    assert!(!said.contains("handed"), "{said}");
}

/// The pane's bar and the `/skills` help both say Enter opens and `s` hands over.
#[test]
fn the_pane_and_help_say_enter_opens_and_s_hands_over() {
    let mut a = app();
    a.session_id = "s1".into();
    a.open_surface(Surface::Skills);
    a.apply(frame(rows()));
    let drawn = a.screen(120, 24).join("\n");
    assert!(
        drawn.contains("enter opens its file, s hands it"),
        "{drawn}"
    );
}
/// The handed-over message draws as one folded row, not the skill's whole body.
#[test]
fn a_handed_over_skill_draws_as_one_folded_row_at_80_columns() {
    let body = (0..40)
        .map(|i| format!("step {i} of the release"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!(
        "[skill `release`, from this project \u{2014} the words below are the skill's, handed to \
         you by the operator]\n{body}\n[end of the skill `release`]\n\ncut 0.5.2"
    );
    let cfg = RenderConfig {
        width: 80,
        color: false,
        ..RenderConfig::default()
    };
    let drawn = crate::ui::transcript::blocks::user_block(&text, 0, &cfg).join("\n");
    assert!(
        drawn.contains("skill `release` handed to the session"),
        "{drawn}"
    );
    assert!(drawn.contains("40 lines"), "{drawn}");
    assert!(drawn.contains("cut 0.5.2"), "{drawn}");
    assert!(!drawn.contains("step 7 of the release"), "{drawn}");
    assert!(drawn.lines().count() <= 4, "{drawn}");
}

/// A hub holding a user row, an offer row and an assistant row, as a head that attaches later
/// gets them in its snapshot.
fn attached_after_an_offer() -> App {
    use letibot_transcript::{OfferKind, Speaker, TranscriptItem, UserPart};
    let hub = Hub::new("s");
    let rows = [
        TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "first question".into(),
            }],
        },
        TranscriptItem::Offer {
            kind: OfferKind::Skill,
            title: "skill offered: rust-skills (0.41) · loaded".into(),
            body: "Step one: run clippy.\nStep two: fmt.".into(),
        },
        TranscriptItem::Assistant {
            text: "the answer".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    ];
    for (n, item) in rows.iter().enumerate() {
        hub.append_row(&format!("s#t1:{n}"), "k", "00", item);
    }
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a
}

/// An offer is a transcript row: a head attaching later draws it in place, between the rows around
/// it and not in the notes, and a click opens its body.
#[test]
fn an_offer_row_from_a_snapshot_draws_in_place_and_a_click_opens_its_body() {
    use super::redcard::{holds, popup_text, rows_with};
    let mut a = attached_after_an_offer();
    let screen = a.screen(100, 30);
    let drawn = screen.join("\n");
    let line = "skill offered: rust-skills (0.41) · loaded · click for details";
    let at = rows_with(&screen, line);
    assert_eq!(at.len(), 1, "{drawn}");
    let y = screen.iter().position(|l| l.contains(line)).unwrap();
    let first = screen.iter().position(|l| l.contains("first question"));
    let last = screen.iter().position(|l| l.contains("the answer"));
    assert!(
        first.is_some_and(|f| f < y) && last.is_some_and(|l| y < l),
        "between the rows it sits between: {drawn}"
    );
    assert!(!drawn.contains("Step one"), "{drawn}");
    assert!(a.notes.is_empty(), "not a note: {:?}", a.notes);
    a.key(Key::Click { x: 10, y: y as u16 });
    let whole = popup_text(&mut a);
    assert!(holds(&whole, "Step one: run clippy."), "{whole}");
}

/// Two offers in a row stay two rows.
#[test]
fn two_offers_close_together_are_two_rows() {
    use super::redcard::rows_with;
    use letibot_transcript::{OfferKind, TranscriptItem};
    let hub = Hub::new("s");
    for (n, t) in [
        "note offered: build.md (0.33)",
        "skill offered: release (0.30)",
    ]
    .into_iter()
    .enumerate()
    {
        let kind = if n == 0 {
            OfferKind::Note
        } else {
            OfferKind::Skill
        };
        let item = TranscriptItem::Offer {
            kind,
            title: t.into(),
            body: "body".into(),
        };
        hub.append_row(&format!("s#t1:{n}"), "offer", "00", &item);
    }
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    let screen = a.screen(100, 30);
    assert_eq!(
        rows_with(&screen, "offered:").len(),
        2,
        "{}",
        screen.join("\n")
    );
}

/// A head attached before the round draws the offer rows the round appends, as they arrive, at the
/// rung that hides the head's working: offers are the operator's, like a delivery.
#[test]
fn offer_rows_appended_live_are_drawn_without_a_snapshot() {
    use super::redcard::rows_with;
    use letibot_sessionlog::event::SessionEvent;
    use letibot_sessionlog::protocol::ServerFrame;
    use letibot_transcript::{OfferKind, Speaker, TranscriptItem, UserPart};
    let mut a = app();
    // The operator's rung: conversation plus edits, the one that hides the head's working.
    a.visibility = Visibility::of(Profile::READ_EDITS);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env_at(
        1,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "m".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    let rows = [
        (
            "user",
            TranscriptItem::User {
                speaker: Speaker::Operator,
                parts: vec![UserPart::Text {
                    text: "first question".into(),
                }],
            },
        ),
        (
            "offer",
            TranscriptItem::Offer {
                kind: OfferKind::Skill,
                title: "skill offered: docx (0.19)".into(),
                body: "Step one".into(),
            },
        ),
        (
            "offer",
            TranscriptItem::Offer {
                kind: OfferKind::Note,
                title: "note offered: build.md (0.33)".into(),
                body: "a note".into(),
            },
        ),
    ];
    let mut seq = 2;
    for (n, (kind, item)) in rows.into_iter().enumerate() {
        let id = format!("s#t1:{n}");
        a.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptAppended {
                item_id: id.clone(),
                kind: kind.into(),
                ledger_head: String::new(),
                item: None,
                body_bytes: 0,
            },
        )));
        a.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id,
                item: Box::new(item),
            },
        )));
        seq += 2;
    }
    let screen = a.screen(100, 30);
    let drawn = screen.join("\n");
    assert_eq!(
        rows_with(&screen, "skill offered: docx (0.19)").len(),
        1,
        "{drawn}"
    );
    assert_eq!(
        rows_with(&screen, "note offered: build.md (0.33)").len(),
        1,
        "{drawn}"
    );
}
