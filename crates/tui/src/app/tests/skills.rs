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
        },
        SkillRow {
            name: "review-a-branch".into(),
            source: "the operator's".into(),
            description: "judge a branch".into(),
        },
        SkillRow {
            name: "plugin-pane".into(),
            source: "this build".into(),
            description: "write a pane plugin".into(),
        },
    ]
}

fn frame(skills: Vec<SkillRow>) -> ServerFrame {
    ServerFrame::Skills {
        session_id: "s1".into(),
        skills,
    }
}

/// **`/skills` opens the pane and asks the daemon**, and the pane draws every row with the source
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

/// **`/skills NAME` hands that skill over**, and the rest of the line goes on as the operator's
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

/// **Enter on a pane row is the same door as the verb** - the row under the cursor, by name.
#[test]
fn enter_on_a_skill_row_hands_that_skill_over() {
    let mut a = app();
    a.session_id = "s1".into();
    a.open_surface(Surface::Skills);
    a.apply(frame(rows()));
    a.screen(120, 40);
    a.key(Key::Down);
    let act = a.key(Key::Enter);
    assert_eq!(
        act,
        Some(Action::LoadSkill {
            name: "review-a-branch".into(),
            then: String::new(),
        })
    );
}
