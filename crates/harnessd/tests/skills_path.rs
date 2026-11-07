//! The whole skill path, walked offline: roots off `Config`, `/skills` listing them, `/skills NAME` handing a body over as the person's turn, `task {skills}` preloading one in a child, and an unknown name refused by name.
//! Every assertion needs no real model.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::sessions::{Outcome, Sessions};
use letibot_harnessd::{Dialect, Parts};
use letibot_sessionlog::hub::{CommandKind, QueuedCommand};
use letibot_sessionlog::registry::{Registry, SessionWiring};
use letibot_tokencore::Vocab;
use letibot_tools::NullToolSink;
use letibot_transcript::{Speaker, ToolCall, TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::{IM_END, THINK_CLOSE};
use support::stub::{read_request, write_frames, write_props};

/// The line in the body that only a load can put in front of the model.
const BODY_MARK: &str = "an outline per section, then one idea per frame";

/// How long a wait for something a correct build produces in milliseconds may last. A ceiling on
/// *the far side never ended*, never a claim about speed.
const PATIENCE: Duration = Duration::from_secs(120);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch(&format!("harnessd-skills-path-{tag}"));
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

fn a_skill(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).expect("a skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: plan a talk\nwhen_to_use: a talk needs an outline\n\
             keywords: talk outline slides\n---\n\n{BODY_MARK}\n"
        ),
    )
    .expect("a skill");
}

fn config(
    store: &Path,
    workspace: &Path,
    id: &str,
    endpoint: Endpoint,
    roots: letibot_tools::builtins::skill::SkillDirs,
) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // `leticode` is the unconfined seat that has `task`: the default seat seats none of the trio,
    // and nothing here asserts a boundary.
    cfg.seat = Seat::Leticode;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = id.to_string();
    cfg.endpoint = endpoint;
    cfg.context_window = Some(32_768);
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    // The box's roots as they are now: named on the config and nowhere else.
    cfg.box_skills = roots;
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

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// A model that answers every turn with a plain reply and keeps every request it was asked.
struct Stub {
    endpoint: Endpoint,
    seen: Arc<Mutex<Vec<String>>>,
    /// Handed over after the fixture's parts exist: a canned turn is spelled in the vocabulary the
    /// session tokenized with.
    scripts: Arc<Mutex<Option<Vec<Vec<Frame>>>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    fn answering() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let scripts: Arc<Mutex<Option<Vec<Vec<Frame>>>>> = Arc::new(Mutex::new(None));
        let (mine, script) = (seen.clone(), scripts.clone());
        let handle = std::thread::spawn(move || {
            // Taken whole on the first turn: a frame list is not `Clone`, and this thread owns what
            // it answers with.
            let mut all: Vec<Vec<Frame>> = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn: a spawn asks for the window with a `GET`.
                if line.starts_with("GET ") {
                    let _ = write_props(stream);
                    continue;
                }
                mine.lock().unwrap().push(body);
                if all.is_empty()
                    && let Some(scripts) = script.lock().unwrap().take()
                {
                    all = scripts;
                }
                let frames = all.first().map(|f| f.as_slice()).unwrap_or_default();
                let _ = write_frames(stream, frames);
            }
        });
        Stub {
            endpoint: Endpoint::new(addr.ip().to_string(), addr.port()),
            seen,
            scripts,
            handle: Some(handle),
        }
    }

    /// The frames every request is answered with, once the vocabulary is known.
    fn script(&self, frames: Vec<Frame>) {
        *self.scripts.lock().unwrap() = Some(vec![frames]);
    }

    fn bodies(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// Every prompt the model was handed, detokenized with the session's own vocabulary.
    fn prompts(&self, vocab: &Vocab) -> Vec<String> {
        self.bodies()
            .iter()
            .map(|body| {
                let json: serde_json::Value = serde_json::from_str(body).expect("JSON");
                let ids: Vec<u32> = json["prompt"]
                    .as_array()
                    .expect("a prompt")
                    .iter()
                    .map(|v| v.as_u64().expect("a token id") as u32)
                    .collect();
                vocab.detokenize(&ids, true).expect("the ids decode")
            })
            .collect()
    }

    /// Wait, bounded, for a prompt this stub has not been asked for yet.
    fn wait_for_prompt(&self, vocab: &Vocab, from: usize, why: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let prompts = self.prompts(vocab);
            if let Some(p) = prompts.get(from) {
                return p.clone();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "{why}: no request {from} within {PATIENCE:?} (asked for {})",
            self.bodies().len()
        );
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // Detached rather than joined: the listener lives as long as the test.
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

/// A turn that thinks nothing, answers `answer` and ends: no tool call, so one round.
fn a_plain_turn(vocab: &Vocab, answer: &str) -> Vec<Frame> {
    let answer_ids = support::qwen::ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: 30,
        processed: 30,
    }];
    frames.extend(support::qwen::spoken(vocab, &[THINK_CLOSE]));
    frames.extend(support::qwen::spoken(vocab, &answer_ids));
    frames.extend(support::qwen::spoken(vocab, &[IM_END]));
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (answer_ids.len() + 2) as u64,
        n_prompt: 30,
        cache_n: 0,
    });
    frames
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

/// One session over a workspace with a project skill and a box skill, opened through `Sessions` the
/// way the daemon opens one.
struct Fixture {
    sessions: Sessions<'static>,
    registry: Arc<Registry>,
    parts: &'static Parts,
    stub: Stub,
    id: String,
    _ws: TempDir,
    _store: TempDir,
    _box_root: TempDir,
}

fn fixture() -> Fixture {
    let ws = TempDir::new("ws");
    let store = TempDir::new("store");
    let box_root = TempDir::new("box");
    a_skill(&ws.path().join(".letibot/skills/handover"), "handover");
    a_skill(&box_root.path().join("box-skill"), "box-skill");
    let stub = Stub::answering();
    let roots = letibot_tools::builtins::skill::SkillDirs {
        operator: vec![box_root.path().to_path_buf()],
        ..Default::default()
    };
    let cfg = config(
        &store.path().join("sessions.db"),
        ws.path(),
        "s-skills-path",
        stub.endpoint.clone(),
        roots,
    );
    let parts: &'static Parts = Box::leak(Box::new(Parts::load(&cfg).expect("the vocabulary")));
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    stub.script(a_plain_turn(&parts.vocab, "done"));
    let registry = Registry::new();
    registry
        .create("s-skills-path", "", wiring(&cfg))
        .expect("the session is in the registry");
    let sessions = Sessions::open_first(parts, cfg, registry.clone()).expect("it opens");
    Fixture {
        sessions,
        registry,
        parts,
        stub,
        id: "s-skills-path".into(),
        _ws: ws,
        _store: store,
        _box_root: box_root,
    }
}

impl Fixture {
    /// One command through the daemon's own dispatch, as a head's key would.
    fn dispatch(&mut self, kind: CommandKind) -> Outcome {
        let cmd = QueuedCommand {
            head_id: "test".into(),
            identity: "dead".into(),
            client_request_id: format!("req-{}", self.stub.bodies().len()),
            at_seq: 0,
            enqueued_ms: 0,
            kind,
        };
        self.sessions.dispatch(&self.id, &cmd)
    }
}

/// The roots come off `Config`, so the box's own directory and the project's are one corpus: what
/// the head's `/skills` lists is that corpus, and never the machine the suite runs on.
#[test]
fn the_roots_on_the_config_are_what_the_skills_list_holds() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let f = fixture();
    let rows = f.registry.skills(&f.id);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"handover"), "the project's root: {names:?}");
    assert!(names.contains(&"box-skill"), "the box's root: {names:?}");
    assert!(
        names.contains(&"plugin-pane"),
        "and the built-ins are still there: {names:?}"
    );
    // The label says whose words each one is, which is the pane's whole point.
    let box_skill = rows
        .iter()
        .find(|r| r.name == "box-skill")
        .expect("box-skill");
    assert_eq!(box_skill.source, "the operator's");
}

/// `/skills NAME rest` hands the body over as the person's own turn, and the rest of the line
/// follows it into the same turn.
#[test]
fn a_handed_over_skill_reaches_the_model_as_the_persons_turn() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let mut f = fixture();
    let vocab = f.parts.vocab.clone();
    let before = f.stub.bodies().len();
    let out = f.dispatch(CommandKind::LoadSkill {
        name: "handover".into(),
        then: "and start now".into(),
    });
    assert!(matches!(out, Outcome::Replied(_)), "the load runs a turn");
    let prompt = f
        .stub
        .wait_for_prompt(&vocab, before, "the handed-over body");
    assert!(
        prompt.contains(BODY_MARK),
        "the body reached the model: {prompt}"
    );
    assert!(
        prompt.contains("and start now"),
        "the rest of the line follows it: {prompt}"
    );
    // The row is the OPERATOR's, because the operator handed it over: a row saying the harness's
    // would authorise nothing and read as the agent talking to itself.
    let items = f
        .sessions
        .harness_of(&f.id)
        .expect("the session is open")
        .items()
        .to_vec();
    let said = items
        .iter()
        .find_map(|i| match i {
            TranscriptItem::User { speaker, parts } if *speaker == Speaker::Operator => {
                parts.iter().find_map(|p| match p {
                    UserPart::Text { text } if text.contains(BODY_MARK) => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("the body is in a row");
    assert!(
        said.contains("[skill `handover`"),
        "the row says whose words those are: {said}"
    );
}

/// A name nothing answers to is refused by name, and no turn is spent on it.
#[test]
fn an_unknown_skill_name_is_refused_and_runs_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let mut f = fixture();
    let before = f.stub.bodies().len();
    let out = f.dispatch(CommandKind::LoadSkill {
        name: "no-such-skill".into(),
        then: String::new(),
    });
    match out {
        Outcome::Failed(why) => assert!(
            why.contains("no-such-skill"),
            "the refusal names the name: {why}"
        ),
        other => panic!(
            "an unknown skill must be refused, not answered: {}",
            matches!(other, Outcome::Replied(_))
        ),
    }
    assert_eq!(
        f.stub.bodies().len(),
        before,
        "a refused name costs no model call"
    );
}

/// `task {skills: [...]}` puts the body in the CHILD's prompt, at its own open - the leg nothing
/// proved end to end before, since the parent's registry and the child's config are joined in the
/// spawn and nowhere else.
#[test]
fn a_task_that_names_a_skill_preloads_it_in_the_child() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let f = fixture();
    let vocab = f.parts.vocab.clone();
    let harness = f.sessions.harness_of(&f.id).expect("the session is open");
    let call = ToolCall {
        id: "call_task".into(),
        name: "task".into(),
        arguments: serde_json::json!({
            "prompt": "plan the talk",
            "skills": ["handover"],
        })
        .to_string(),
    };
    let started = harness
        .runtime_handle()
        .invoke("turn_1", &call, &mut NullToolSink);
    let handle = match started.outcome {
        letibot_transcript::ToolOutcome::Backgrounded { handle, .. } => handle,
        other => panic!("the task did not start: {other:?}"),
    };
    // The child's open is where the body lands, and it is the second request this stub sees.
    let deadline = Instant::now() + PATIENCE;
    let mut found = None;
    while Instant::now() < deadline {
        for prompt in f.stub.prompts(&vocab) {
            if prompt.contains(BODY_MARK) && prompt.contains("plan the talk") {
                found = Some(prompt);
                break;
            }
        }
        if found.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let prompt = found.unwrap_or_else(|| {
        panic!(
            "the child was never handed the skill's body ({} request(s))",
            f.stub.bodies().len()
        )
    });
    assert!(
        prompt.contains("the skills this session was started with"),
        "the block is the preloaded one: {prompt}"
    );
    let _ = handle;
    // A name nothing answers to is refused at the CALL, so a child is never spawned against a
    // list nothing could vouch for.
    let bad = ToolCall {
        id: "call_task_2".into(),
        name: "task".into(),
        arguments: serde_json::json!({
            "prompt": "plan the talk",
            "skills": ["ghost-skill"],
        })
        .to_string(),
    };
    let refused = harness
        .runtime_handle()
        .invoke("turn_1", &bad, &mut NullToolSink);
    let said = format!("{:?}", refused.outcome);
    assert!(
        said.contains("ghost-skill"),
        "the refusal names the name: {said}"
    );
}
