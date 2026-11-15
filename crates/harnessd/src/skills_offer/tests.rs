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
        edits: Vec::new(),
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

/// A skill asked for in none of its words. `keynote` and `deck` are in no frontmatter here,
/// so the words score the subject at zero; the second tier's cosine, over the two planted as
/// synonyms of `beamer` and `slides`, earns the offer once the session has named the skill.
#[test]
fn a_subject_in_none_of_the_skills_words_is_offered_by_the_second_tier() {
    let registry = SkillRegistry::new(vec![
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
    ]);
    let text = subject(&prompt("a keynote deck outline"), &[]);

    let mut lexical = Offers::new();
    assert!(
        !lexical.compose(&registry, &text, None),
        "the words share nothing with any frontmatter"
    );

    let data = std::env::temp_dir().join(format!(
        "harnessd-skills-tier-{}-{}",
        std::process::id(),
        now_secs()
    ));
    let tier =
        letibot_embeddings::testing::planted(&data, &[&["beamer", "keynote"], &["slides", "deck"]]);
    let mut offers = Offers::new().with_tier(Some(tier));
    assert!(
        !offers.compose(&registry, &text, None),
        "meaning alone, with no word of the skill's name or keywords said, is not evidence"
    );
    // The operator said `slides` earlier; the subject is still only the deck outline.
    offers.observe(&prompt("we need slides for friday"));
    assert!(
        offers.compose(&registry, &text, None),
        "the meaning earns the offer"
    );
    let made = offers.tail();
    let said = match &made[0] {
        TranscriptItem::User { parts, .. } => match &parts[0] {
            UserPart::Text { text } => text.clone(),
            other => panic!("a text part, not {other:?}"),
        },
        other => panic!("a user-side item, not {other:?}"),
    };
    assert!(said.contains("latex-presentation"), "{said}");
    assert!(said.contains("its meaning"), "{said}");
    let _ = std::fs::remove_dir_all(&data);
}

/// The prompt alone is enough: a person asking for a presentation is offered the skill that makes
/// one, and the corpus's own subject decides which one.
#[test]
fn a_prompt_about_slides_composes_the_offer_of_the_slides_skill() {
    let mut offers = Offers::new();
    assert!(offers.compose(
        &registry(),
        &subject(&prompt("make me a latex presentation for the talk"), &[]),
        None,
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

/// One name is one line. Two skills sharing a name are one skill to every door that keys by
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
        None,
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
        offers.compose(&registry(), &text, None),
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

/// The plan survives a tool result that fills the subject. The rows on the board are the work in
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
        None,
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
            &subject(&prompt("a latex presentation please"), &[]),
            None
        ),
        "a skill already offered this session is not offered again"
    );
    // And another session is its own memory.
    let mut other = Offers::new();
    other.recall(&dir, "s-2");
    assert!(other.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[]),
        None
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
        &subject(&prompt("a latex presentation please"), &[]),
        None
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
    assert!(offers.compose(&many, "beamer slides, a latex presentation", None));
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
            "how warm should the kitchen be for a sourdough starter",
            None
        ),
        "nothing above the floor, so nothing is said"
    );
    assert!(silent.tail().is_empty());
    // And an empty subject is nothing to score.
    assert!(!Offers::new().compose(&registry(), "  \n ", None));
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
            &subject(&prompt("a latex presentation please"), &[]),
            None
        ),
        "the model loaded it, so it needs no offer"
    );
    // The declared list is the other half: a `task {skills}` child's bodies arrive in its prompt.
    let mut declared = Offers::new();
    declared.skip_names(["latex-presentation".to_string()].iter());
    assert!(!declared.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[]),
        None
    ));
    // And another skill is still offered, so the skip is a name and not the whole offer.
    let mut other = Offers::new();
    other.skip_names(["latex-presentation".to_string()].iter());
    assert!(other.compose(
        &registry(),
        &subject(&prompt("cut a tagged release please"), &[]),
        None
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

/// Skills that name tools the seat lacks are not offered, a plain one still is, `never_offer`
/// vetoes by name, and the reason lands in the log.
#[test]
fn a_skill_the_seat_cannot_use_is_not_offered() {
    let claude = |name: &str, body: &str| {
        Skill::new(
            name,
            "desktop screenshots of the screen and apps, click and type",
            body,
            SkillSource::Installed(Some("claude".into())),
        )
    };
    let mut all = registry().skills;
    all.extend(vec![
        claude(
            "computer-use",
            "Its tools are named `mcp__computer-use__*` when in the desktop app.",
        ),
        claude(
            "chrome-browser",
            "Tools named `mcp__claude-in-chrome__*` act in Chrome.",
        ),
        claude("docs", "Everything is served by the docs connector."),
        claude("plain", "Take care when you click and type."),
        claude("vetoed", "Take care when you click and type."),
    ]);
    let registry = SkillRegistry::new(all);
    let text = subject(
        &prompt("take desktop screenshots of the screen and apps, click and type"),
        &[],
    );
    let tools: Vec<String> = vec!["bash".into(), "read".into()];
    let never = ["vetoed".to_string()];
    let mut offers = Offers::new().with_seat(tools.iter(), never.iter());
    // Every candidate is considered: ask repeatedly as the cap is two.
    let mut seen = Vec::new();
    for _ in 0..4 {
        if !offers.compose(&registry, &text, None) {
            break;
        }
        for o in &offers.open {
            seen.push(o.name.clone());
        }
        let logs = std::env::temp_dir().join(format!(
            "harnessd-skills-seat-{}-{}",
            std::process::id(),
            now_secs()
        ));
        offers.settle(&logs, "s1", &[]);
        let log = std::fs::read_to_string(logs.join(LOG)).unwrap_or_default();
        if log.contains("skipped") {
            assert!(
                log.contains("computer-use") && log.contains("mcp__computer-use__"),
                "{log}"
            );
            assert!(log.contains("never_offer"), "{log}");
            assert!(log.contains("docs"), "{log}");
        }
    }
    assert_eq!(seen.iter().filter(|n| *n == "plain").count(), 1, "{seen:?}");
    for bad in ["computer-use", "chrome-browser", "docs", "vetoed"] {
        assert!(!seen.iter().any(|n| n == bad), "{bad} offered: {seen:?}");
    }

    let with: Vec<String> = vec!["mcp__computer-use__screenshot".into()];
    let seat = with.iter().map(|t| t.to_ascii_lowercase()).collect();
    let skills = registry.live_skills();
    let cu = skills.iter().find(|s| s.name == "computer-use").unwrap();
    assert!(cu.unmet(&seat).is_none(), "present tools satisfy it");
}

/// The operator's real synced Claude Code skills, when this box has them: a plain seat is offered
/// none that name tools it lacks. Skips loudly where the files are absent.
#[test]
fn the_real_synced_claude_skills_are_not_offered_to_a_plain_seat() {
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("SKIPPED: no HOME");
        return;
    };
    let synced = PathBuf::from(home).join(".claude/skills/synced");
    let roots: Vec<PathBuf> = std::fs::read_dir(&synced)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    let loaded = SkillRegistry::load_from_dirs(&roots);
    let skills = loaded.live_skills();
    let seat: std::collections::BTreeSet<String> = ["bash", "read"].map(String::from).into();
    let mut checked = 0;
    for name in ["computer-use", "chrome-browser", "docs"] {
        match skills.iter().find(|s| s.name == name) {
            Some(skill) => {
                checked += 1;
                let why = skill.unmet(&seat);
                assert!(why.is_some(), "{name} would be offered to a plain seat");
                eprintln!("{name}: {}", why.unwrap());
            }
            None => eprintln!("SKIPPED: {name} is not under {}", synced.display()),
        }
    }
    if checked == 0 {
        eprintln!("SKIPPED: no synced Claude Code skills on this box");
    }
}

/// Two skills answer one job and only one of them fits this seat: the offer names the one that
/// fits and never the other, whatever it scores - and the log says why the other went unnamed.
#[test]
fn an_offer_skips_the_skill_whose_capability_this_seat_lacks() {
    let page = Skill {
        when_to_use: Some("the operator asks for a dashboard or a status page".into()),
        keywords: vec!["dashboard".into(), "status".into(), "monitor".into()],
        requires: vec!["flowy".into()],
        ..Skill::new(
            "instrument-dashboard",
            "a status page served over the web",
            "write html and serve it",
            SkillSource::Installed(Some("npx skills add".into())),
        )
    };
    let pane = Skill {
        when_to_use: Some("the operator asks for a dashboard or a status page".into()),
        keywords: vec!["dashboard".into(), "status".into(), "panel".into()],
        ..Skill::new(
            "plugin-pane",
            "a pane drawn in this head",
            "scaffold a pane",
            SkillSource::BuiltIn,
        )
    };
    let registry = SkillRegistry::new(vec![
        page,
        pane,
        // A third subject, so the words the two share are not in every document: idf of a term in
        // the whole corpus is zero, and a test whose two skills are the whole corpus would score
        // both at 0 and prove nothing.
        Skill::new(
            "release",
            "cut a tagged release",
            "tag, push, write the notes",
            SkillSource::Operator,
        ),
    ]);
    let text = subject(
        &prompt("the operator asks for a dashboard or a status page: build me one"),
        &[],
    );

    let without = ["read".to_string(), "bash".to_string()];
    let mut offered = Offers::new().with_seat(without.iter(), std::iter::empty::<&String>());
    assert!(
        offered.compose(&registry, &text, None),
        "the pane still fits"
    );
    let names: Vec<&str> = offered.open.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["plugin-pane"], "the page is not offered: {names:?}");

    // And the refusal is measured, not silent: the log carries the seat's own sentence.
    let logs = std::env::temp_dir().join(format!(
        "harnessd-skills-when-{}-{}",
        std::process::id(),
        now_secs()
    ));
    offered.settle(&logs, "s1", &[]);
    let log = std::fs::read_to_string(logs.join(LOG)).unwrap_or_default();
    assert!(
        log.contains("instrument-dashboard")
            && log.contains("requires `flowy`, which this seat does not have"),
        "the skipped line names the skill and the reason: {log}"
    );

    // With flowy on the seat the page is offered beside the pane: the rule is the capability and
    // nothing else.
    let with = ["read".into(), "flowy".into()];
    let mut both = Offers::new().with_seat(with.iter(), std::iter::empty::<&String>());
    assert!(both.compose(&registry, &text, None));
    assert!(
        both.open.iter().any(|o| o.name == "instrument-dashboard"),
        "a seat with the capability is offered the page: {:?}",
        both.open.iter().map(|o| &o.name).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&logs);
}

// ---- The operator's own prompt, scored against the skills' keywords ----

/// The build's own frontmatter, compiled in from `skills/`. The case this suite was filed on is
/// `plugin-pane`, and a fixture standing in for its keywords would prove nothing about the words
/// the skill publishes.
fn built_in() -> SkillRegistry {
    SkillRegistry::new(SkillRegistry::built_in())
}

/// The round the offer did not fire in: a fuzzing plan, a page of seeds and crashes, and the
/// operator's one short line under all of it. Neither the plan nor the result says "pane".
fn fuzzing_round() -> (String, &'static str) {
    let words = "ok please add it as a pane";
    let mut items = prompt(words);
    items.push(result(
        "seed 3181: no crash after 4000 mutations of the parser's input corpus",
    ));
    items.push(answer(
        "the campaign has run 2 000 seeds; the smallest reproducer is 41 bytes",
    ));
    let plan = [
        row("fuzz the parser with 4000 random seeds until it crashes"),
        row("triage the crashes and reduce the smallest reproducer"),
    ];
    (subject(&items, &plan), words)
}

/// The offer the operator's line has to earn on its own: one keyword, a short prompt, and a subject
/// that has buried it under a campaign's worth of tool output.
#[test]
fn a_short_prompt_about_a_pane_is_heard_over_a_fuzzing_subject() {
    let (text, words) = fuzzing_round();
    let mut offers = Offers::new();
    assert!(
        offers.compose(&built_in(), &text, Some(words)),
        "the prompt asked for a pane and nothing was offered: {text}"
    );
    let names: Vec<&str> = offers.open.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names.first(), Some(&"plugin-pane"), "{names:?}");
    assert_eq!(
        offers.open[0].by,
        By::Prompt,
        "the prompt's own score is what earned it: {:?}",
        offers.open[0]
    );
    // The line says which arithmetic scored it: a tail may not claim a document it did not read.
    let said = tail_text(&offers);
    assert!(said.contains("keywords"), "{said}");
}

/// The measurement the rule rests on, against the real frontmatter: one word the skill publishes,
/// scored against `keywords` alone, clears the floor the subject has to clear - and a skill that
/// publishes none of the prompt's words scores zero rather than being dragged in with it.
#[test]
fn one_keyword_in_a_short_prompt_scores_over_the_floor_on_its_own() {
    let ranked = built_in().ranked_by_keywords("ok please add it as a pane", 8);
    let (_, scored) = ranked
        .iter()
        .find(|(name, _)| name == "plugin-pane")
        .unwrap_or_else(|| panic!("the pane's own keywords were not ranked: {ranked:?}"));
    assert!(
        *scored >= OFFER_FLOOR,
        "one keyword scored {scored:.3} of 1.00, under the {OFFER_FLOOR} floor"
    );
    assert!(
        !ranked.iter().any(|(name, _)| name == "screen"),
        "the screen tool publishes none of these words: {ranked:?}"
    );
}

/// A skill offered and not taken may be offered once more - by a new prompt that names it, because
/// asking again is evidence the subject coming back never is.
#[test]
fn an_untaken_offer_comes_back_on_a_new_prompt_and_not_on_the_same_one() {
    let dir = logs("asked-again");
    let registry = built_in();
    let (text, words) = fuzzing_round();
    let mut offers = Offers::new();
    assert!(offers.compose(&registry, &text, Some(words)));
    assert_eq!(offers.open[0].name, "plugin-pane");
    // The round did nothing with it.
    offers.settle(&dir, "s-1", &[]);
    // The same words on the next round of the same turn, and a round with no prompt at all.
    assert!(
        !offers.compose(&registry, &text, Some(words)),
        "the same request twice is a nag"
    );
    assert!(
        !offers.compose(&registry, &text, None),
        "a round with no new operator prompt is not new evidence"
    );
    // A new prompt that names it is.
    let second = "make it a pane";
    assert!(
        offers.compose(&registry, &text, Some(second)),
        "the operator asked again and was not heard: {:?}",
        offers.open
    );
    assert!(
        offers.open.iter().any(|o| o.name == "plugin-pane"),
        "{:?}",
        offers.open
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A body the session holds is never offered again, however the operator's words move - a load is
/// the take-up the whole rule is about, and the two doors that notice one both count.
#[test]
fn a_taken_offer_is_never_offered_again_however_the_prompt_words_move() {
    let dir = logs("taken-prompt");
    let registry = built_in();
    let (text, words) = fuzzing_round();
    let mut offers = Offers::new();
    assert!(offers.compose(&registry, &text, Some(words)));
    offers.settle(&dir, "s-1", &[a_load("plugin-pane")]);
    assert!(
        !offers.compose(&registry, &text, Some("add the panel to the dashboard")),
        "a skill the round loaded was offered again: {:?}",
        offers.open
    );
    // And a session that restarts remembers it: the log line said the round loaded it.
    let mut after = Offers::new();
    after.recall(&dir, "s-1");
    assert!(
        !after.compose(&registry, &text, Some("make it a pane")),
        "the log's take-up is the memory: {:?}",
        after.open
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A load the transcript shows is the same fact as a settled one, and it stops a prompt from
/// re-offering the body the model is already holding.
#[test]
fn a_body_the_transcript_shows_loaded_is_not_offered_by_a_prompt_either() {
    let loaded = vec![TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![a_load("plugin-pane")],
        truncated: false,
    }];
    let mut offers = Offers::new();
    offers.skip_loaded(&loaded);
    assert!(!offers.compose(&built_in(), &fuzzing_round().0, Some("make it a pane")));
}

/// The take-up log says which of the two scores earned the line: a ratio over both would be two
/// different measurements added together.
#[test]
fn the_log_says_which_score_earned_the_offer() {
    let dir = logs("by");
    let (text, words) = fuzzing_round();
    let mut offers = Offers::new();
    assert!(offers.compose(&built_in(), &text, Some(words)));
    offers.settle(&dir, "s-1", &[]);
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(
        logged.contains("\"skill\":\"plugin-pane\"") && logged.contains("\"by\":\"prompt\""),
        "{logged}"
    );

    // The other side, on a caller that passed no prompt: the subject's own offer says so.
    let mut subject_only = Offers::new();
    assert!(subject_only.compose(
        &registry(),
        &subject(&prompt("a latex presentation please"), &[]),
        None
    ));
    subject_only.settle(&dir, "s-2", &[]);
    let logged = std::fs::read_to_string(dir.join(LOG)).expect("the log");
    assert!(
        logged.contains("\"skill\":\"latex-presentation\"")
            && logged.contains("\"by\":\"subject\""),
        "{logged}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The one line this round's request carries, as the model reads it.
fn tail_text(offers: &Offers) -> String {
    let tail = offers.tail();
    let Some(TranscriptItem::User { parts, .. }) = tail.first() else {
        panic!("an offer is a user item: {tail:?}")
    };
    match &parts[0] {
        UserPart::Text { text } => text.clone(),
        other => panic!("a text part, not {other:?}"),
    }
}

/// The latex fixture's subject, as an operator would ask for it.
fn latex() -> String {
    subject(&prompt("a latex presentation please"), &[])
}

/// Rows of one kind in a log, parsed.
fn rows_with(log: &Path, key: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r.get(key).is_some())
        .collect()
}

/// An offer the model does not load in its own round or the two after it is declined: the
/// session log says so, the project log counts it, and the session does not hear it again.
#[test]
fn an_offer_not_loaded_in_three_rounds_is_declined() {
    let dir = logs("declined");
    let project = dir.join("project");
    let mut offers = Offers::new().with_project(&project);
    assert!(offers.compose(&registry(), &latex(), None));
    offers.settle(&dir, "s-1", &[]);
    offers.settle(&dir, "s-1", &[]);
    assert!(
        rows_with(&dir.join(LOG), "declined").is_empty(),
        "two rounds is not yet"
    );
    offers.settle(&dir, "s-1", &[]);
    let declined = rows_with(&dir.join(LOG), "declined");
    assert_eq!(declined.len(), 1, "{declined:?}");
    assert_eq!(declined[0]["skill"], "latex-presentation");
    let counted = Declines::read(&project, "skill");
    assert_eq!(counted.total("latex-presentation"), 1);
    assert!(!offers.compose(&registry(), &latex(), None));
    // A restart reads the decline back as one, not as an offer still waiting.
    let mut after = Offers::new().with_project(&project);
    after.recall(&dir, "s-1");
    assert!(after.pending.is_empty(), "{:?}", after.pending);
    assert!(after.declined.contains_key("latex-presentation"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A load one round late is a take-up, logged as one, and never counted as a decline.
#[test]
fn a_late_load_is_a_take_up_and_not_a_decline() {
    let dir = logs("late");
    let mut offers = Offers::new().with_project(&dir);
    assert!(offers.compose(&registry(), &latex(), None));
    offers.settle(&dir, "s-1", &[]);
    offers.settle(&dir, "s-1", &[a_load("latex-presentation")]);
    offers.settle(&dir, "s-1", &[]);
    offers.settle(&dir, "s-1", &[]);
    let late = rows_with(&dir.join(LOG), "late");
    assert_eq!(late.len(), 1, "{late:?}");
    assert!(rows_with(&dir.join(LOG), "declined").is_empty());
    assert_eq!(Declines::read(&dir, "skill").total("latex-presentation"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An offer a turn ended on is still waiting in the next turn, which declines it if it is not loaded.
#[test]
fn an_offer_left_open_by_a_turn_is_declined_in_the_next() {
    let dir = logs("carried");
    let mut first = Offers::new().with_project(&dir);
    assert!(first.compose(&registry(), &latex(), None));
    first.settle(&dir, "s-1", &[]);
    let mut next = Offers::new().with_project(&dir);
    next.recall(&dir, "s-1");
    next.settle(&dir, "s-1", &[]);
    next.settle(&dir, "s-1", &[]);
    assert_eq!(rows_with(&dir.join(LOG), "declined").len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each decline of a skill in this project raises its floor, so a score that cleared the plain
/// floor stops clearing it once the model has passed the skill over often enough.
#[test]
fn declines_in_the_project_raise_the_skills_floor() {
    let dir = logs("raised");
    let mut plain = Offers::new().with_project(&dir);
    assert!(plain.compose(&registry(), &latex(), None));
    let score = plain.settle(&dir, "s-0", &[a_load("latex-presentation")])[0].score;
    let needed = ((score - OFFER_FLOOR) / crate::offer_evidence::DECLINE_STEP).floor() as u32 + 1;
    let mut declines = Declines::read(&dir, "skill");
    for i in 0..needed {
        declines.record(
            &format!("s-{i}"),
            "latex-presentation",
            &version_of(&slides()),
            0,
        );
    }
    let mut raised = Offers::new().with_project(&dir);
    assert!(
        !raised.compose(&registry(), &latex(), None),
        "{score} under a floor raised by {needed} declines"
    );
    // Another project's declines are not this one's.
    let mut elsewhere = Offers::new().with_project(&dir.join("other"));
    assert!(elsewhere.compose(&registry(), &latex(), None));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A declined skill comes back in the same session only when the session names it in a word it
/// had not used when the skill was declined.
#[test]
fn a_declined_skill_comes_back_on_new_evidence_only() {
    let dir = logs("renewed");
    let data = dir.join("data");
    let tier =
        letibot_embeddings::testing::planted(&data, &[&["beamer", "keynote"], &["slides", "deck"]]);
    let mut offers = Offers::new().with_tier(Some(tier)).with_project(&dir);
    let deck = subject(&prompt("a keynote deck outline"), &[]);
    offers.observe(&prompt("we need slides for friday"));
    assert!(offers.compose(&registry(), &deck, None));
    for _ in 0..DECLINE_ROUNDS {
        offers.settle(&dir, "s-1", &[]);
    }
    assert_eq!(rows_with(&dir.join(LOG), "declined").len(), 1);
    assert!(
        !offers.compose(&registry(), &deck, None),
        "the same evidence again"
    );
    let mut more = prompt("we need slides for friday");
    more.extend(prompt("in beamer, not powerpoint"));
    offers.observe(&more);
    assert!(
        offers.compose(&registry(), &deck, None),
        "`beamer` is a word the decline was not made on"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The built-in MiniLM checkpoint, from `LETIBOT_TEST_MINILM` or this box's data directory.
/// `None` skips the test: the suite never downloads a model.
fn minilm() -> Option<PathBuf> {
    let dir = std::env::var_os("LETIBOT_TEST_MINILM")
        .map(PathBuf::from)
        .or_else(|| {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
                })?;
            Some(letibot_embeddings::builtin_dir(&data.join("letibot")))
        })?;
    dir.join("model.safetensors").is_file().then_some(dir)
}

/// The skills the rano session was offered, and two it could have used, as their frontmatter reads.
fn fleet() -> SkillRegistry {
    let skill = |name: &str, description: &str| {
        Skill::new(name, description, "the body", SkillSource::Operator)
    };
    SkillRegistry::new(vec![
        skill(
            "fuzz-harness-writing",
            "How to write an in-process fuzz harness for a chosen surface: init versus per-input \
             work, reset, the real caller's duties, input encoding, seeds, dictionaries, guards.",
        ),
        skill(
            "fuzzing-method",
            "How to fuzz properly: separate harness bugs from bugs in the system under test, \
             budget by execution rate, triage crashes, prove a fix.",
        ),
        skill(
            "conformity-check-writing",
            "How to write a check for a protocol conformity tester: observation, differ, \
             normalizer, fixtures, deadlines and tests that prove a check can fail.",
        ),
        skill(
            "pane-screenshot",
            "See a terminal pane the way the operator sees it: render a tmux pane to a PNG with \
             colours, bold and reverse video, and read the image.",
        ),
    ])
}

/// The rano case on the real model: a TUI session fixing touchpad scroll, with clippy output in
/// hand, is offered no fuzzing or conformity skill.
#[test]
fn an_unrelated_subject_is_not_offered_under_the_real_tier() {
    let Some(model) = minilm() else {
        eprintln!("SKIPPED: no MiniLM checkpoint; set LETIBOT_TEST_MINILM");
        return;
    };
    let data = logs("minilm-unrelated");
    let settings = letibot_embeddings::Settings {
        mode: letibot_embeddings::Mode::Internal { model: Some(model) },
        download: false,
        ..Default::default()
    };
    let tier = letibot_embeddings::Tier::open(&settings, &data)
        .expect("the tier opens")
        .expect("internal");
    let mut items = prompt("well my touchpad still doesnt scroll");
    items.push(reasoning(
        "Gate green. The full suite ran under the harness with every check passing; commit, land \
         and push, then tell the operator to restart and try the touchpad.",
    ));
    items.push(result(
        "FMT-OK\n    Checking rano v0.10.1\n    Finished `dev` profile\nrunning 1349 tests\n\
         test term::tests::alt_screen_restores ... ok\ntest editor::tests::wheel_scrolls_the_bottom \
         ... ok\ntest pty::tests::a_crash_in_the_child_is_reported ... ok\ntest result: ok. 1349 \
         passed; 0 failed; 0 ignored\nGATE-OK",
    ));
    let mut offers = Offers::new().with_tier(Some(tier));
    offers.share_round(Some(subject(&items, &[])));
    offers.observe(&items);
    offers.compose(&fleet(), &subject(&items, &[]), None);
    let named: Vec<String> = offers.open.iter().map(|o| o.name.clone()).collect();
    assert!(
        named.iter().all(|n| n == "pane-screenshot"),
        "offered {named:?}"
    );

    // And the related subject still is: the operator asked for a fuzz harness.
    let mut asked = prompt("write a fuzz harness for the config parser");
    asked.push(reasoning(
        "An LLVMFuzzerTestOneInput that feeds the bytes to the toml loader, resetting state per input.",
    ));
    offers.share_round(Some(subject(&asked, &[])));
    offers.observe(&asked);
    assert!(offers.compose(&fleet(), &subject(&asked, &[]), None));
    let named: Vec<String> = offers.open.iter().map(|o| o.name.clone()).collect();
    assert!(
        named.contains(&"fuzz-harness-writing".to_string()),
        "offered {named:?}"
    );
    let _ = std::fs::remove_dir_all(&data);
}

/// The latex skill after an edit to its body: the same name, another version.
fn edited() -> SkillRegistry {
    let mut skill = slides();
    skill.body = "two ideas to a frame".into();
    with_others(skill)
}

/// `skill` beside the fixture's two others: a corpus of one weighs every word at nothing.
fn with_others(skill: Skill) -> SkillRegistry {
    let mut all = registry().skills;
    all[0] = skill;
    SkillRegistry::new(all)
}

/// A decline is of one version. The skill edited since is offered once more, a second decline of
/// the edit holds, and the project's count for the edit starts again.
#[test]
fn an_edited_skill_loses_its_decline_once() {
    let dir = logs("edited");
    let mut offers = Offers::new().with_project(&dir);
    assert!(offers.compose(&registry(), &latex(), None));
    for _ in 0..DECLINE_ROUNDS {
        offers.settle(&dir, "s-1", &[]);
    }
    assert!(!offers.compose(&registry(), &latex(), None), "declined");
    assert!(offers.compose(&edited(), &latex(), None), "edited since");
    for _ in 0..DECLINE_ROUNDS {
        offers.settle(&dir, "s-1", &[]);
    }
    assert!(
        !offers.compose(&edited(), &latex(), None),
        "the edit was declined too"
    );
    let declines = Declines::read(&dir, "skill");
    assert_eq!(declines.total("latex-presentation"), 2);
    let edit = version_of(&edited().live_skills()[0]);
    assert_eq!(edited().live_skills()[0].name, "latex-presentation");
    assert_eq!(declines.count("latex-presentation", &edit), 1);
}

/// A decline does not cover a skill that did not exist when it was made: another skill that took
/// the declined name is a new document.
#[test]
fn a_decline_does_not_cover_a_skill_that_took_the_name_later() {
    let dir = logs("renamed");
    let mut offers = Offers::new().with_project(&dir);
    assert!(offers.compose(&registry(), &latex(), None));
    for _ in 0..DECLINE_ROUNDS {
        offers.settle(&dir, "s-1", &[]);
    }
    let other = with_others(Skill {
        keywords: vec!["latex".into(), "presentation".into()],
        ..Skill::new(
            "latex-presentation",
            "a latex presentation from a markdown outline",
            "pandoc first",
            SkillSource::Project,
        )
    });
    assert!(offers.compose(&other, &latex(), None));
}

/// A skill this session wrote is never offered back to it.
#[test]
fn a_skill_this_session_wrote_is_not_offered_back() {
    let root = logs("wrote");
    let skills = root.join("skills");
    let md = skills.join("latex-presentation/SKILL.md");
    std::fs::create_dir_all(md.parent().unwrap()).unwrap();
    std::fs::write(
        &md,
        "---\nname: latex-presentation\ndescription: beamer slides for a talk\nkeywords: latex, beamer, slides\n---\n\none idea to a frame\n",
    )
    .unwrap();
    let registry = SkillRegistry::load_for(&letibot_tools::builtins::skill::SkillDirs {
        operator: vec![skills.clone()],
        ..Default::default()
    });
    let mut offers = Offers::new();
    assert!(
        offers.compose(&registry, &latex(), None),
        "offered to a session that did not write it"
    );
    let mut items = prompt("a latex presentation please");
    items.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: "call_w".into(),
            name: "write".into(),
            arguments: serde_json::json!({"path": md.display().to_string(), "content": "x"})
                .to_string(),
        }],
        truncated: false,
    });
    let mut writer = Offers::new();
    writer.observe(&items);
    assert!(!writer.compose(&registry, &latex(), None));
    let _ = std::fs::remove_dir_all(&root);
}

fn pick(name: &str) -> Pick {
    Pick {
        name: name.into(),
        quote: "make the talk slides".into(),
    }
}

/// The shadow's picks become the round's offers, with its quote in the tail and the log, and the
/// rules no model may loosen still hold: hidden, vetoed, loaded, unknown, offered once, at most two.
#[test]
fn the_shadows_picks_are_the_offers_under_the_same_rules() {
    let mut all = registry().skills;
    all.push(Skill::new(
        "kept",
        "the operator's own",
        "b",
        SkillSource::Operator,
    ));
    let registry = SkillRegistry::new(all).with_hidden(["kept"]);
    let never = ["release".to_string()];
    let tools: Vec<String> = Vec::new();
    let mut offers = Offers::new().with_seat(tools.iter(), never.iter());
    offers.skip_names(["postgres-migrations".to_string()].iter());
    let picks = [
        pick("kept"),
        pick("release"),
        pick("postgres-migrations"),
        pick("no-such-skill"),
        pick("latex-presentation"),
    ];
    assert!(offers.adopt(&registry, &picks, "s1-shadow"));
    let names: Vec<&str> = offers.open.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["latex-presentation"]);
    let tail = format!("{:?}", offers.tail());
    assert!(tail.contains("shadow reviewer picked it"), "{tail}");
    assert!(tail.contains("make the talk slides"), "{tail}");
    let logs = std::env::temp_dir().join(format!(
        "harnessd-skills-adopt-{}-{}",
        std::process::id(),
        now_secs()
    ));
    offers.settle(&logs, "s1", &[]);
    let log = std::fs::read_to_string(logs.join(LOG)).unwrap();
    assert!(log.contains("\"by\":\"shadow\""), "{log}");
    assert!(log.contains("never_offer"), "the veto is logged: {log}");
    // Offered once: the same pick next round says nothing.
    assert!(!offers.adopt(&registry, &[pick("latex-presentation")], "s1-shadow"));
    assert!(offers.tail().is_empty());
    let _ = std::fs::remove_dir_all(&logs);
}

/// An empty pick is no offer, even when similarity would have offered: the shadow decides.
#[test]
fn an_empty_pick_offers_nothing_where_similarity_would() {
    let registry = registry();
    let text = subject(&prompt("make latex beamer slides for the talk"), &[]);
    assert!(
        Offers::new().compose(&registry, &text, None),
        "similarity offers"
    );
    let mut offers = Offers::new();
    assert!(!offers.adopt(&registry, &[], "s1-shadow"));
    assert!(offers.tail().is_empty());
}
