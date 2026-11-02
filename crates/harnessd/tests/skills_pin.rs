//! `[skills] pin` in a `leticode.toml`, read by the real loader: a pin is listed in the prompt, both
//! levels' pins apply, and a pin that answers to nothing is said at open by name rather than silently
//! dropped.

use letibot_harnessd::config::Config;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::Hub;

mod support;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-skills-pin-{tag}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn a_skill(workspace: &std::path::Path, name: &str, line: &str) {
    let dir = workspace.join(".letibot/skills").join(name);
    std::fs::create_dir_all(&dir).expect("a skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {line}\n---\n\nthe body of {name}\n"),
    )
    .expect("a skill");
}

/// A session over `workspace`, opened the way the daemon opens one: the project's `leticode.toml`
/// read by the one loader and merged into the config `cli::run` builds.
fn opened(
    workspace: &std::path::Path,
    store: &std::path::Path,
    id: &str,
) -> (Harness, std::sync::Arc<Hub>) {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = id.to_string();
    cfg.vocab_gguf = support::vocab::gguf();
    // The box's own roots are not this test's business: the project's are the workspace's.
    cfg.box_skills = letibot_tools::builtins::skill::SkillDirs::default();
    cfg.leticode =
        letibot_harnessd::leticode_config::LeticodeConfig::load(&workspace.join("leticode.toml"))
            .expect("the project's file parses");
    cfg.seat_prompt = Some(letibot_harnessd::config::SeatPrompt::default());
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let hub = Hub::new(id);
    let h = Harness::open(&parts, cfg, hub.clone()).expect("the session opens");
    (h, hub)
}

/// A pinned skill is listed in the prompt by name and an unpinned one is not: the pin is what puts
/// a row there, and nothing else does.
#[test]
fn a_pinned_skill_is_in_the_prompt_and_an_unpinned_one_is_not() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws");
    let store = TempDir::new("store");
    a_skill(ws.path(), "release", "cut a tagged release");
    a_skill(ws.path(), "review", "judge a branch");
    std::fs::write(
        ws.path().join("leticode.toml"),
        "[skills]\npin = [\"release\"]\n",
    )
    .expect("the project's file");
    let (h, _hub) = opened(ws.path(), &store.path().join("sessions.db"), "s-pin");
    let system = &h.config().system;
    assert!(
        !system.contains("names no skill"),
        "the pin answers to a skill, so nothing is said about it: {system}"
    );
    assert!(
        system.contains("- release"),
        "a pinned skill is listed in the prompt: {system}"
    );
    assert!(
        !system.contains("- review"),
        "an unpinned skill is a count, not a row: {system}"
    );
    assert!(
        !system.contains("the body of release"),
        "the pin is not a load: the body is not in the prompt: {system}"
    );
}

/// A pin that names no skill is said at open, by name - the operator's typo must not read as a
/// pin that took effect.
#[test]
fn a_pin_that_names_no_skill_is_said_at_open() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-ghost");
    let store = TempDir::new("store-ghost");
    a_skill(ws.path(), "release", "cut a tagged release");
    std::fs::write(
        ws.path().join("leticode.toml"),
        "[skills]\npin = [\"ghost-skill\"]\n",
    )
    .expect("the project's file");
    let (_h, hub) = opened(ws.path(), &store.path().join("sessions.db"), "s-pin-ghost");
    let warned: Vec<String> = hub
        .retained()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::Warning { code, detail, .. } if code == "skills_pin" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(warned.len(), 1, "one row, said once: {warned:?}");
    assert!(
        warned[0].contains("ghost-skill"),
        "the pin is named so the typo can be found: {}",
        warned[0]
    );
}

/// The operator's own file and the project's both apply: neither level can unsay the other's pins.
#[test]
fn the_projects_pin_and_the_operators_pin_both_apply() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-both");
    let store = TempDir::new("store-both");
    let user = TempDir::new("user-both");
    a_skill(ws.path(), "release", "cut a tagged release");
    a_skill(ws.path(), "house-style", "the operator's prose");
    std::fs::write(
        ws.path().join("leticode.toml"),
        "[skills]\npin = [\"release\"]\n",
    )
    .expect("the project's file");
    let user_file = user.path().join("leticode.toml");
    std::fs::write(&user_file, "[skills]\npin = [\"house-style\"]\n").expect("the user's file");
    let mut cfg = Config::for_this_box(ws.path());
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.path().join("sessions.db"));
    cfg.session_id = "s-pin-both".into();
    cfg.vocab_gguf = support::vocab::gguf();
    cfg.box_skills = letibot_tools::builtins::skill::SkillDirs::default();
    let user_level = letibot_harnessd::leticode_config::LeticodeConfig::load_as(
        &user_file,
        letibot_harnessd::leticode_config::Level::User,
    )
    .expect("the user's file parses");
    let project_level =
        letibot_harnessd::leticode_config::LeticodeConfig::load(&ws.path().join("leticode.toml"))
            .expect("the project's file parses");
    cfg.leticode = user_level.overridden_by(&project_level);
    let parts = Parts::load(&cfg).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let hub = Hub::new("s-pin-both");
    let h = Harness::open(&parts, cfg, hub).expect("the session opens");
    let system = &h.config().system;
    assert!(
        system.contains("- house-style") && system.contains("- release"),
        "both levels' pins are listed: {system}"
    );
}
