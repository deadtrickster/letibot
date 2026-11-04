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

/// A skill offered to the model draws as one faint row, and a click opens its body.
#[test]
fn an_offered_skill_draws_as_a_row_and_a_click_opens_its_body() {
    use super::redcard::{holds, popup_text, rows_with, with_warning};
    let detail =
        "skill offered: rust-skills (0.41) · loaded\n\nStep one: run clippy.\nStep two: fmt.";
    let mut a = with_warning("skill_offer", detail);
    let screen = a.screen(100, 30);
    let drawn = screen.join("\n");
    assert_eq!(
        rows_with(
            &screen,
            "skill offered: rust-skills (0.41) · loaded · click for details"
        )
        .len(),
        1,
        "{drawn}"
    );
    assert!(!drawn.contains("Step one"), "{drawn}");
    assert_eq!(a.card_rows.len(), 1, "{drawn}");
    let (y, _) = a.card_rows[0];
    a.key(Key::Click { x: 10, y: y as u16 });
    let whole = popup_text(&mut a);
    assert!(holds(&whole, "Step one: run clippy."), "{whole}");
}

/// Two offers in a minute stay two rows, not a counted burst.
#[test]
fn two_offers_close_together_are_two_rows() {
    use super::redcard::{rows_with, with_warning};
    let mut a = with_warning("note_offer", "note offered: build.md (0.33)\n\nbody");
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Warning {
            code: "skill_offer".into(),
            detail: "skill offered: release (0.30)".into(),
            compaction: None,
        },
    )));
    let screen = a.screen(100, 30);
    assert_eq!(
        rows_with(&screen, "offered:").len(),
        2,
        "{}",
        screen.join("\n")
    );
}
