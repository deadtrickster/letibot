//! The offer, against a fixture corpus and a fixture transcript: no model, no network, no harness.
//! What this module does not do is the delivery - the request tail is `harness/turns.rs`'s, and the
//! turn that proves it is `tests/skills_offer.rs`.

use super::*;
use std::path::PathBuf;

use letibot_tokencore::store::{TodoBy, TodoStatus};
use letibot_tools::builtins::skill::{Skill, SkillSource};
use letibot_transcript::ToolOutcome;

/// A skill whose frontmatter is about slides, the subject the fixtures reach by its keywords.
fn slides() -> Skill {
    Skill {
        when_to_use: Some("the person wants a latex presentation or beamer slides".into()),
        keywords: vec!["latex".into(), "beamer".into(), "slides".into()],
        ..Skill::new(
            "latex-presentation",
            "beamer slides for a talk",
            "one idea to a frame",
            SkillSource::Project,
        )
    }
}

fn registry() -> SkillRegistry {
    SkillRegistry::new(vec![
        slides(),
        Skill::new(
            "postgres-migrations",
            "change a live schema safely",
            "backfill in batches",
            SkillSource::Project,
        ),
        Skill::new(
            "release",
            "cut a tagged release",
            "tag, push, write the notes",
            SkillSource::Operator,
        ),
    ])
}

fn prompt(text: &str) -> Vec<TranscriptItem> {
    vec![TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }]
}

fn reasoning(text: &str) -> TranscriptItem {
    TranscriptItem::Reasoning {
        text: text.into(),
        field: letibot_transcript::ReasoningField::Inline,
        truncated: false,
    }
}

fn answer(text: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: text.into(),
        tool_calls: Vec::new(),
        truncated: false,
    }
}

fn result(payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "call_1".into(),
        name: "read".into(),
        outcome: ToolOutcome::Ok,
        payload: payload.into(),
        edit: None,
        origin: None,
        media: None,
    }
}

fn row(content: &str) -> TodoItem {
    TodoItem {
        content: content.into(),
        status: TodoStatus::Pending,
        by: TodoBy::Model,
        when: None,
        needs: Vec::new(),
    }
}

fn a_load(name: &str) -> ToolCall {
    ToolCall {
        id: "call_skill".into(),
        name: "skill".into(),
        arguments: format!("{{\"action\":\"load\",\"name\":\"{name}\"}}"),
    }
}

/// A directory that removes itself, for the log.
fn logs(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "harnessd-skills-offer-{name}-{}-{}",
        std::process::id(),
        now_secs()
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// The prompt alone is enough: a person asking for a presentation is offered the skill that makes
/// one, and the corpus's own subject decides which one.
#[test]
fn a_prompt_about_slides_composes_the_offer_of_the_slides_skill() {
    let mut offers = Offers::new();
    assert!(offers.compose(
        &registry(),
        &subject(&prompt("make me a latex presentation for the talk"), &[]),
    ));
    let made = offers.tail();
    assert_eq!(made.len(), 1, "{made:?}");
    let TranscriptItem::User { parts, speaker } = &made[0] else {
        panic!("an offer is a user item: {made:?}")
    };
    assert_eq!(
        *speaker,
        Speaker::Agent,
        "the harness's own line, and it says so"
    );
    let UserPart::Text { text } = &parts[0] else {
        panic!("text")
    };
    assert!(text.contains("latex-presentation"), "{text}");
    assert!(text.contains("action=\"load\""), "{text}");
    assert!(
        !text.contains("one idea to a frame"),
        "never the body: {text}"
    );
}

/// **One name is one line.** Two skills sharing a name are one skill to every door that keys by
/// name - a pin, a `skill load`, an offer - so a round offers it once rather than twice.
#[test]
fn two_skills_with_one_name_are_offered_once() {
    // The same skill twice, over the fixture's corpus: two identical documents in a corpus of two
    // make every term common and nothing distinctive, which is a different test.
    let mut skills = registry().skills.clone();
    skills.push(Skill {
        source: SkillSource::Installed(None),
        ..slides()
    });
    let twice = SkillRegistry::new(skills);
    let mut offers = Offers::new();
    assert!(offers.compose(
        &twice,
        &subject(&prompt("make me a latex presentation for the talk"), &[]),
    ));
    let made = offers.tail();
    assert_eq!(made.len(), 1, "one name, one line: {made:?}");
}

/// What the model has worked out counts, and a note's subject does not read it: the reasoning of the
/// round before is what makes the next round's offer.
#[test]
fn the_models_reasoning_and_text_are_part_of_the_subject() {
    let items = vec![
        TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "carry on with what we discussed".into(),
            }],
        },
        reasoning("this wants beamer slides, one frame per section"),
        answer("planning the deck"),
    ];
    let text = subject(&items, &[]);
    assert!(text.contains("beamer slides"), "{text}");
    assert!(text.contains("planning the deck"), "{text}");
    assert!(
        text.contains("carry on"),
        "the prompt is still half of it: {text}"
    );
    let mut offers = Offers::new();
    assert!(
        offers.compose(&registry(), &text),
        "the reasoning's words reach the skill: {text}"
    );
}

/// The plan is a subject too: a row the model wrote says what the work is about even when neither
/// the prompt nor the last answer does.
#[test]
fn the_plan_on_the_board_is_part_of_the_subject() {
    let queue = TodoItem {
        by: TodoBy::Queue,
        ..row("review agent/queue-row on the merge queue")
    };
    let text = subject(
        &prompt("keep going"),
        &[row("build the beamer slides for the offsite"), queue],
    );
    assert!(text.contains("beamer slides"), "{text}");
    assert!(
        !text.contains("merge queue"),
        "the queue's rows are the repository's, not this session's plan: {text}"
    );
}

/// **The plan survives a tool result that fills the subject.** The rows on the board are the work in
/// hand, and a cut that kept only the newest end of the walk dropped them in exactly the rounds a
/// tool result was longest - which are the rounds a skill is wanted in.
#[test]
fn the_plan_survives_a_tool_result_that_fills_the_subject() {
    let mut items = prompt("keep going");
    items.push(result(&"x".repeat(SUBJECT_CHARS * 2)));
    let text = subject(&items, &[row("build the beamer slides for the offsite")]);
    assert!(
        text.contains("beamer slides"),
        "the plan is what the round is about, and the result cut it: {} bytes",
        text.len()
    );
    assert!(text.len() <= SUBJECT_CHARS, "{} bytes", text.len());
}

/// The tail is settled against the round's calls and logged either way: an offer taken is a `skill`
/// load of that name, and the ratio is the only thing that tunes the floor.
#[test]
fn a_round_settles_its_offers_into_the_log_and_a_load_is_the_take_up() {
    let dir = logs("settle");
    let mut offers = Offers::new();
    assert!(offers.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[]),
    ));
    let settled = offers.settle(&dir, "s-1", &[a_load("latex-presentation")]);
    assert_eq!(settled.len(), 1, "{settled:?}");
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(
        logged.contains("\"skill\":\"latex-presentation\"")
            && logged.contains("\"taken\":true")
            && logged.contains("\"session\":\"s-1\""),
        "{logged}"
    );
    // The memory is the log, so a session that restarts does not offer it again.
    let mut after = Offers::new();
    after.recall(&dir, "s-1");
    assert!(
        !after.compose(
            &registry(),
            &subject(&prompt("a latex presentation please"), &[])
        ),
        "a skill already offered this session is not offered again"
    );
    // And another session is its own memory.
    let mut other = Offers::new();
    other.recall(&dir, "s-2");
    assert!(other.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[])
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A round that took nothing is logged as one: the ratio needs both halves.
#[test]
fn an_untaken_offer_is_logged_as_untaken() {
    let dir = logs("untaken");
    let mut offers = Offers::new();
    assert!(offers.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[])
    ));
    offers.settle(&dir, "s-2", &[]);
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(logged.contains("\"taken\":false"), "{logged}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// At most two, and nothing at all below the floor: the line is worth reading or it is not written.
#[test]
fn the_offer_names_at_most_two_and_says_nothing_under_the_floor() {
    // Three decks and two skills about something else: a corpus where every document carries a
    // term weighs that term at nothing, so the decks alone would score zero against each other.
    let many = SkillRegistry::new(vec![
        Skill::new(
            "postgres-migrations",
            "change a live schema safely",
            "backfill in batches",
            SkillSource::Project,
        ),
        Skill::new(
            "release",
            "cut a tagged release",
            "tag, push, write the notes",
            SkillSource::Operator,
        ),
        slides(),
        Skill {
            keywords: vec!["beamer".into(), "slides".into()],
            ..Skill::new("deck-a", "a deck of slides", "a body", SkillSource::Project)
        },
        Skill {
            keywords: vec!["beamer".into(), "slides".into()],
            ..Skill::new("deck-b", "another deck", "a body", SkillSource::Project)
        },
    ]);
    let mut offers = Offers::new();
    assert!(offers.compose(&many, "beamer slides, a latex presentation"));
    let made = offers.tail();
    assert!(
        made.len() <= OFFERS_MAX,
        "{} offers is a wall: {made:?}",
        made.len()
    );

    let mut silent = Offers::new();
    assert!(
        !silent.compose(
            &registry(),
            "how warm should the kitchen be for a sourdough starter"
        ),
        "nothing above the floor, so nothing is said"
    );
    assert!(silent.tail().is_empty());
    // And an empty subject is nothing to score.
    assert!(!Offers::new().compose(&registry(), "  \n "));
}

/// A body the session already holds is not offered again: a load the model made itself and a skill it
/// was STARTED with are both in the conversation, and either one offered again is noise.
#[test]
fn a_body_the_session_already_holds_is_not_offered() {
    let loaded = vec![TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![a_load("latex-presentation")],
        truncated: false,
    }];
    let mut offers = Offers::new();
    offers.skip_loaded(&loaded);
    assert!(
        !offers.compose(
            &registry(),
            &subject(&prompt("a latex presentation please"), &[])
        ),
        "the model loaded it, so it needs no offer"
    );
    // The declared list is the other half: a `task {skills}` child's bodies arrive in its prompt.
    let mut declared = Offers::new();
    declared.skip_names(["latex-presentation".to_string()].iter());
    assert!(!declared.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[])
    ));
    // And another skill is still offered, so the skip is a name and not the whole offer.
    let mut other = Offers::new();
    other.skip_names(["latex-presentation".to_string()].iter());
    assert!(other.compose(
        &registry(),
        &subject(&prompt("cut a tagged release please"), &[])
    ));
}

/// A `find` that named a skill is a search, not a use: the take-up is the load that put the body in
/// the conversation.
#[test]
fn only_a_load_is_a_take_up() {
    let find = ToolCall {
        id: "c".into(),
        name: "skill".into(),
        arguments: r#"{"action":"find","query":"latex presentation"}"#.into(),
    };
    let list = ToolCall {
        id: "c".into(),
        name: "skill".into(),
        arguments: r#"{"action":"list"}"#.into(),
    };
    assert_eq!(loaded_of(&a_load("release")).as_deref(), Some("release"));
    assert!(loaded_of(&find).is_none(), "a find is not a load");
    assert!(loaded_of(&list).is_none());
    let read = ToolCall {
        id: "c".into(),
        name: "read".into(),
        arguments: r#"{"path":".letibot/skills/x/SKILL.md"}"#.into(),
    };
    assert!(loaded_of(&read).is_none());
}

/// The subject is capped from the recent end, and the cut never lands inside a character.
#[test]
fn the_subject_is_capped_from_the_newest_end() {
    let long = "beamer ".repeat(SUBJECT_CHARS);
    let items = vec![
        TranscriptItem::User {
            speaker: Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "the oldest thing said".into(),
            }],
        },
        result(&long),
        reasoning("the newest thought"),
    ];
    let text = subject(&items, &[]);
    assert!(text.len() <= SUBJECT_CHARS + 256, "{} bytes", text.len());
    assert!(text.contains("the newest thought"), "the newest survives");
    assert!(!text.contains("the oldest thing said"), "the oldest is cut");
}
