//! A skill that says what it needs of a seat: `requires`/`unless` in its own frontmatter, and
//! `[skills.when.<name>]` in the project's `leticode.toml` over it. One session, the byte
//! vocabulary and no model: the four doors - the pane's listing, `list`, `find` and `load` - are
//! walked over a real open and must give one answer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::Sessions;
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tools::NullToolSink;
use letibot_transcript::{ToolCall, ToolOutcome};
use letibot_turn::Endpoint;

/// The line only the body of `panel` carries, so a load is a fact about the text.
const PANEL_BODY: &str = "one number to a cell, and its denominator";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch(&format!("harnessd-skills-when-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn a_skill(workspace: &Path, name: &str, front: &str, body: &str) {
    let dir = workspace.join(".letibot/skills").join(name);
    std::fs::create_dir_all(&dir).expect("a skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: the {name} one\n{front}---\n\n{body}\n"),
    )
    .expect("a skill");
}

fn config(store: &Path, workspace: &Path, id: &str) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // `leticode` is the seat with a shell and the read tools, which is the seat this is about.
    cfg.seat = Seat::Leticode;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = id.to_string();
    cfg.endpoint = Endpoint::new("127.0.0.1", 1);
    cfg.http_retries = 0;
    // The byte vocabulary: nothing in this test asks a model for a token, and the box's own GGUF
    // is not this suite's business.
    cfg.vocab_gguf = None;
    // The box's own roots are not this test's business: the project's are the workspace's.
    cfg.box_skills = letibot_tools::builtins::skill::SkillDirs::default();
    cfg.leticode =
        letibot_harnessd::leticode_config::LeticodeConfig::load(&workspace.join("leticode.toml"))
            .expect("the project's file parses");
    cfg.seat_prompt = Some(letibot_harnessd::config::SeatPrompt::default());
    cfg
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// The workspace: three skills that answer one job, each gated a different way, and a project file
/// that re-gates one of them.
fn workspace() -> TempDir {
    let ws = TempDir::new("ws");
    // Hidden by its own frontmatter: this seat has no flowy.
    a_skill(
        ws.path(),
        "instrument-dashboard",
        "when_to_use: a dashboard or a status page\nrequires: flowy\n",
        "serve html over the web",
    );
    // Shown, because `unless` hides on PRESENCE: flowy is not here.
    a_skill(
        ws.path(),
        "panel",
        "when_to_use: a dashboard or a status page\nunless: flowy\n",
        PANEL_BODY,
    );
    // The frontmatter needs `read`, which every seat has; the operator's file says `flowy`.
    a_skill(
        ws.path(),
        "slides",
        "when_to_use: a dashboard is a kind of slide\nrequires: read\n",
        "one idea to a frame",
    );
    std::fs::write(
        ws.path().join("leticode.toml"),
        "[skills.when.slides]\nrequires = [\"flowy\"]\n",
    )
    .expect("the project's file");
    ws
}

struct Fixture {
    sessions: Sessions<'static>,
    registry: Arc<Registry>,
    id: String,
    _ws: TempDir,
    _store: TempDir,
}

fn fixture() -> Fixture {
    let ws = workspace();
    let store = TempDir::new("store");
    let cfg = config(
        &store.path().join("sessions.db"),
        ws.path(),
        "s-skills-when",
    );
    let parts: &'static Parts = Box::leak(Box::new(Parts::load(&cfg).expect("the vocabulary")));
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    registry
        .create("s-skills-when", "", wiring(&cfg))
        .expect("the session is in the registry");
    let sessions = Sessions::open_first(parts, cfg, registry.clone()).expect("it opens");
    Fixture {
        sessions,
        registry,
        id: "s-skills-when".into(),
        _ws: ws,
        _store: store,
    }
}

impl Fixture {
    /// One call, as the engine would make it, rendered the way the model reads it.
    fn call(&self, args: &str) -> (ToolOutcome, String) {
        let harness = self
            .sessions
            .harness_of(&self.id)
            .expect("the session is open");
        let mut sink = NullToolSink;
        let result = harness.runtime_handle().invoke(
            "turn_1",
            &ToolCall {
                id: "call_skill".into(),
                name: "skill".into(),
                arguments: args.into(),
            },
            &mut sink,
        );
        let said = result.render();
        (result.outcome, said)
    }

    fn listed(&self) -> Vec<String> {
        self.registry
            .skills(&self.id)
            .into_iter()
            .map(|r| r.name)
            .collect()
    }
}

/// The pane's listing is the seat's answer: a skill gated by its frontmatter and one gated by the
/// operator's file are both absent, the one that fits this seat is present, and the built-ins are
/// not filtered by a rule nobody wrote for them.
#[test]
fn the_listing_hides_the_skills_this_seat_cannot_use_and_keeps_the_rest() {
    let f = fixture();
    // The `flowy` DOOR is seated on every root session, and it is not a capability: nothing is
    // behind it, so a skill that needs the seat is hidden even though the tool name is here.
    let facts = f
        .sessions
        .harness_of(&f.id)
        .expect("the session is open")
        .config()
        .seat_facts
        .clone()
        .expect("the seat's facts are recorded at open");
    assert!(
        facts.tools.contains("flowy"),
        "this is the interesting case: the tool name is on the seat and the seat is still not one"
    );
    let names = f.listed();
    assert!(
        names.contains(&"panel".to_string()),
        "`unless: flowy` fits a seat without flowy: {names:?}"
    );
    assert!(
        !names.contains(&"instrument-dashboard".to_string()),
        "`requires: flowy` does not: {names:?}"
    );
    assert!(
        !names.contains(&"slides".to_string()),
        "the operator's table replaced a frontmatter that this seat satisfied: {names:?}"
    );
    assert!(
        names.contains(&"plugin-pane".to_string()),
        "a built-in with no condition is listed: {names:?}"
    );
}

/// `skill load` refuses with the reason rather than handing the body over, and a skill that fits
/// still loads: the refusal is the skill's condition and not a broken door.
#[test]
fn a_load_refuses_the_reason_and_not_only_the_name() {
    let f = fixture();
    let (outcome, said) = f.call(r#"{"action":"load","name":"instrument-dashboard"}"#);
    assert!(
        matches!(outcome, ToolOutcome::Failed { .. }),
        "the load is refused: {said}"
    );
    assert!(
        said.contains("requires `flowy`, which this seat does not have"),
        "the refusal carries the reason: {said}"
    );
    assert!(
        !said.contains("serve html over the web"),
        "and no part of the body reached the model: {said}"
    );

    let (outcome, said) = f.call(r#"{"action":"load","name":"slides"}"#);
    assert!(
        matches!(outcome, ToolOutcome::Failed { .. }),
        "the operator's table is what the tool asks: {said}"
    );
    assert!(said.contains("`flowy`"), "{said}");

    let (outcome, said) = f.call(r#"{"action":"load","name":"panel"}"#);
    assert!(matches!(outcome, ToolOutcome::Ok), "{said}");
    assert!(said.contains(PANEL_BODY), "the body arrives: {said}");
}

/// `list` names the reason beside the skill, so a model choosing between two that answer one job
/// can see which one this seat has - the whole point of the rule.
#[test]
fn the_listing_names_the_reason_a_skill_is_not_for_this_seat() {
    let f = fixture();
    let (_, said) = f.call(r#"{"action":"list"}"#);
    assert!(
        said.contains("not for this seat") && said.contains("requires `flowy`"),
        "{said}"
    );
    assert!(
        said.contains("- panel  [this project]"),
        "and the usable half is listed as before: {said}"
    );
}

/// `find` scores every name and then keeps the ones this seat can use: a gated skill that scores
/// highest is skipped, and its body is never what a query answers with.
#[test]
fn a_find_never_answers_with_a_skill_this_seat_cannot_use() {
    let f = fixture();
    let (outcome, said) = f.call(r#"{"action":"find","query":"build me a dashboard"}"#);
    assert!(
        !said.contains("serve html over the web"),
        "the gated skill's body is not the answer: {said}"
    );
    if matches!(outcome, ToolOutcome::Ok) {
        assert!(
            said.contains("the panel one") || said.contains(PANEL_BODY),
            "what it did answer with is a skill this seat has: {said}"
        );
    }
}
