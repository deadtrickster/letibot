use super::*;

use std::collections::BTreeMap;

use letibot_tools::builtins::skill::{SkillSource, SkillWhen};

fn registry(pins: &[&str]) -> Arc<SkillRegistry> {
    let policy = Skill {
        policy: true,
        ..Skill::new(
            "rust-builds",
            "how to build Rust on this box",
            "CARGO_TARGET_DIR goes under /build.",
            SkillSource::Installed(None),
        )
    };
    let help = Skill::new(
        "git-help",
        "git tips",
        "rebase often.",
        SkillSource::Installed(None),
    );
    let when = |w: &str| SkillWhen {
        command: vec![w.into()],
        ..SkillWhen::default()
    };
    Arc::new(
        SkillRegistry::new(vec![policy, help])
            .with_when(BTreeMap::from([
                ("rust-builds".to_string(), when("cargo")),
                ("git-help".to_string(), when("git")),
            ]))
            .with_pins(pins.iter().copied()),
    )
}

fn bash(command: &str) -> letibot_plugin::Call {
    letibot_plugin::Call::new("bash", serde_json::json!({ "command": command }))
}

fn answer(p: &Patched) -> String {
    match p.out.verdict() {
        letibot_plugin::tool_call::Verdict::Answer(t) => t.to_string(),
        other => panic!("not an answer: {other:?}"),
    }
}

/// The first `cargo` call without the skill is held and answered with the rules, worded as rules;
/// the next one runs.
#[test]
fn the_first_cargo_call_gets_the_policy_text_once() {
    let hook = PolicyHook::new(registry(&[]), Delivered::default(), None);
    let standing = Standing::default();
    assert!(hook.before(&bash("ls -la"), &standing).is_none());
    let held = hook
        .before(&bash("nice -n 10 cargo test -p x"), &standing)
        .expect("the first cargo call is held");
    let text = answer(&held);
    assert!(text.starts_with(MARK), "{text}");
    assert!(text.contains("`rust-builds`") && text.contains("every `cargo` call"));
    assert!(text.contains("not a suggestion") && text.contains("/build"));
    assert!(text.contains("did not run"));
    assert_eq!(held.by, BY);
    assert!(
        hook.before(&bash("cargo build"), &standing).is_none(),
        "the rules are given once per session"
    );
}

/// A non-policy skill's `command` is not a trigger: only policy is delivered unasked.
#[test]
fn a_non_policy_skill_is_never_delivered_by_a_command() {
    let hook = PolicyHook::new(registry(&[]), Delivered::default(), None);
    assert!(
        hook.before(&bash("git status"), &Standing::default())
            .is_none()
    );
}

/// A session that loaded the skill, or holds it from a pin or a previous delivery, is not held.
#[test]
fn a_skill_already_held_is_not_delivered_again() {
    let loaded = PolicyHook::new(registry(&[]), Delivered::default(), None);
    let load = letibot_plugin::Call::new(
        "skill",
        serde_json::json!({ "action": "load", "name": "rust-builds" }),
    );
    assert!(loaded.before(&load, &Standing::default()).is_none());
    assert!(
        loaded
            .before(&bash("cargo test"), &Standing::default())
            .is_none()
    );

    // A restored session reads its earlier delivery back off the transcript.
    let skills = registry(&[]);
    let text = rules(&skills.skills[0], "every `cargo` call");
    let restored = Delivered::default();
    restored.seed(&[TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Agent,
        parts: vec![letibot_transcript::UserPart::Text { text }],
    }]);
    assert!(restored.has("rust-builds"));
}

/// A pinned policy rides the prompt whole as standing rules, and a non-policy pin does not.
#[test]
fn a_pinned_policy_is_a_rules_block() {
    let block = pinned_block(&registry(&["rust-builds", "git-help"])).expect("a block");
    assert!(block.contains("### standing rules for this box"), "{block}");
    assert!(block.contains("#### rust-builds") && block.contains("/build"));
    assert!(!block.contains("git-help"));
    assert!(pinned_block(&registry(&["git-help"])).is_none());
}
