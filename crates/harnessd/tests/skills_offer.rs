//! A skill offers itself in the request's tail, the way a note does. The turns are driven through
//! the whole production path with the model replaced by a stub, and the assertion is on the bytes the
//! model was handed: an offer never becomes a row, so only the wire can show it.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use letibot_harnessd::config::Config;
use letibot_harnessd::sessions::Outcome;
use letibot_harnessd::{Dialect, Parts, Sessions};
use letibot_sessionlog::hub::{CommandKind, Hub};
use letibot_sessionlog::registry::Registry;
use letibot_tokencore::Vocab;
use letibot_turn::Endpoint;

mod support;

use support::canned::Frame;
use support::qwen::{IM_END, THINK_CLOSE, spoken};
use support::stub::{read_request, write_frames, write_props};

/// The words only the offer can put in a prompt - the notes' own line starts differently, and no
/// row this fixture writes carries this sentence.
const OFFER_MARK: &str = "[skill offer";

/// The skill the fixtures hold: a subject no other word in the turn reaches, so a match is a match
/// and not a word both halves happen to share.
const SLIDES: &str = "latex-presentation";

/// The tokens Qwen spells a tool call with. A round that calls anything at all is a round with a
/// boundary, which is the only place a second round's offer can be composed.
const TOOL_CALL_OPEN: u32 = 248058;
const TOOL_CALL_CLOSE: u32 = 248059;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "harnessd-skills-offer-{tag}-{}-{}",
            std::process::id(),
            letibot_harnessd::config::now_ns()
        ));
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

/// A workspace's own skills: the one the fixtures are about, and one decoy, so a corpus of one is
/// not what the score is measured against.
fn skills_in(workspace: &Path) {
    let slides = workspace.join(".letibot/skills").join(SLIDES);
    std::fs::create_dir_all(&slides).expect("a skill dir");
    std::fs::write(
        slides.join("SKILL.md"),
        "---\nname: latex-presentation\ndescription: beamer slides for a talk, built from an \
         outline\nwhen_to_use: the person wants a latex presentation, a slide deck, or beamer \
         slides\nkeywords: latex beamer slides presentation deck talk\n---\n\n\
         Write the deck as beamer frames, one idea to a frame.\n",
    )
    .expect("the slides skill");
    let pg = workspace.join(".letibot/skills/postgres-migrations");
    std::fs::create_dir_all(&pg).expect("a skill dir");
    std::fs::write(
        pg.join("SKILL.md"),
        "---\nname: postgres-migrations\ndescription: change a live database schema safely\n\
         when_to_use: a migration must run while the table is being written to\n\
         keywords: postgres sql migration backfill lock\n---\n\nbackfill in batches\n",
    )
    .expect("the migrations skill");
}

fn config(store: &Path, workspace: &Path, session_id: &str, endpoint: Endpoint) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = session_id.to_string();
    cfg.endpoint = endpoint;
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.http_retries = 0;
    cfg.vocab_gguf = support::vocab::gguf();
    // The box's own skill roots are not this test's business: the project's two conventions are
    // the workspace's, and they are the fixture's.
    cfg.box_skills = letibot_tools::builtins::skill::SkillDirs::default();
    cfg
}

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// A model that answers with one script per request, and keeps every request it was asked. The
/// script is handed over after the session's parts exist, because a canned turn is spelled in the
/// vocabulary the session tokenized with.
struct Stub {
    endpoint: Endpoint,
    seen: Arc<Mutex<Vec<String>>>,
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
            let mut n = 0usize;
            // Taken whole on the first turn, because a frame list is not `Clone` and the thread
            // owns what it answers with.
            let mut all: Vec<Vec<Frame>> = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (line, body) = read_request(stream.try_clone().expect("clone the socket"));
                // `/props` is not a turn: a session asks its endpoint for the window with a `GET`.
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
                let at = n.min(all.len().saturating_sub(1));
                let frames = all.get(at).map(|f| f.as_slice()).unwrap_or_default();
                n += 1;
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

    /// The frames to answer with, one list per request; the last list answers any further request.
    fn script(&self, scripts: Vec<Vec<Frame>>) {
        *self.scripts.lock().unwrap() = Some(scripts);
    }

    /// What the model was handed for request `n`, detokenized with the session's own vocabulary.
    fn prompt_of(&self, n: usize, vocab: &Vocab) -> String {
        let seen = self.seen.lock().unwrap();
        let body = seen
            .get(n)
            .cloned()
            .unwrap_or_else(|| panic!("request {n} was never asked for (asked: {})", seen.len()));
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("request {n} is JSON: {e}"));
        let ids: Vec<u32> = json["prompt"]
            .as_array()
            .unwrap_or_else(|| panic!("request {n} carries a prompt: {body}"))
            .iter()
            .map(|v| v.as_u64().expect("a token id") as u32)
            .collect();
        vocab
            .detokenize(&ids, true)
            .unwrap_or_else(|e| panic!("request {n}'s ids decode: {e}"))
    }

    fn turns(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // Detached rather than joined: the listener lives as long as the test, and joining would
        // block on a socket nobody will connect to again.
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

// ---------------------------------------------------------------------------
// The rounds
// ---------------------------------------------------------------------------

/// A turn that thinks `thought`, calls a tool and ends - so the turn has a round boundary.
fn a_tool_call_turn(vocab: &Vocab, thought: &str, function: &str, n_prompt: u64) -> Vec<Frame> {
    let thought_ids = support::qwen::ids(vocab, thought);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.push(Frame::Token {
        id: TOOL_CALL_OPEN,
        text: "<tool_call>",
    });
    let call =
        format!("\n<function={function}>\n<parameter=why>\nno reason\n</parameter>\n</function>\n");
    let call_ids = support::qwen::ids(vocab, &call);
    frames.extend(spoken(vocab, &call_ids));
    frames.push(Frame::Token {
        id: TOOL_CALL_CLOSE,
        text: "</tool_call>",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        // The thought, the two tool-call fences and the closing `</function>` line: every
        // generated id, which the engine checks rather than reconstructs.
        n_decoded: (thought_ids.len() + call_ids.len() + 3) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// A turn that answers in plain text and ends: no tool call, so one round.
fn an_answer_turn(vocab: &Vocab, thought: &str, answer: &str, n_prompt: u64) -> Vec<Frame> {
    let thought_ids = support::qwen::ids(vocab, thought);
    let answer_ids = support::qwen::ids(vocab, answer);
    let mut frames = vec![Frame::Progress {
        total: n_prompt,
        processed: n_prompt,
    }];
    frames.extend(spoken(vocab, &thought_ids));
    frames.push(Frame::Token {
        id: THINK_CLOSE,
        text: "",
    });
    frames.extend(spoken(vocab, &answer_ids));
    frames.push(Frame::Token {
        id: IM_END,
        text: "",
    });
    frames.push(Frame::Final {
        stop_type: "eos",
        n_decoded: (thought_ids.len() + answer_ids.len() + 2) as u64,
        n_prompt,
        cache_n: 0,
    });
    frames
}

/// The engine borrows these for the session's whole life, so a test opens them once per fixture.
fn parts_for(cfg: &Config) -> Parts {
    let parts = Parts::load(cfg).expect("the vocabulary must load");
    *parts.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    parts
}

/// Open a session over `workspace`, hand it `prompt`, and run the turn to its end.
fn drive<'a>(parts: &'a Parts, cfg: &Config, id: &str, prompt: &str) -> (Sessions<'a>, Arc<Hub>) {
    let registry = Registry::new();
    registry
        .create(id, "", Sessions::wiring(cfg))
        .expect("the session is in the registry");
    let mut sessions =
        Sessions::open_first(parts, cfg.clone(), registry.clone()).expect("it opens");
    let hub = registry.get(id).expect("the hub");
    let head = hub.attach(
        "tui",
        "dead",
        letibot_sessionlog::protocol::Caps::default(),
        0,
    );
    hub.submit(
        &head.head_id,
        "p1",
        0,
        CommandKind::Prompt {
            text: prompt.into(),
            attachments: Vec::new(),
        },
    );
    let cmd = hub.try_command().expect("the prompt is queued");
    let outcome = sessions.dispatch(id, &cmd);
    if let Outcome::Failed(why) = &outcome {
        panic!("the turn failed: {why}");
    }
    assert!(matches!(outcome, Outcome::Replied(_)), "the turn ran");
    (sessions, hub)
}

/// The person asks for a presentation, and the skill whose frontmatter is about presentations is
/// offered before the model has said a word. Its body is not in the prompt: the offer is a name.
#[test]
fn a_prompt_asking_for_a_presentation_is_offered_the_skill_that_makes_one() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-prompt");
    let store = TempDir::new("store-prompt");
    skills_in(ws.path());
    let stub = Stub::answering();
    let cfg = config(
        &store.path().join("sessions.db"),
        ws.path(),
        "s-offer-prompt",
        stub.endpoint.clone(),
    );
    let parts = parts_for(&cfg);
    stub.script(vec![an_answer_turn(
        &parts.vocab,
        "a short answer",
        "here is the deck plan",
        30,
    )]);
    let (_sessions, _hub) = drive(
        &parts,
        &cfg,
        "s-offer-prompt",
        "make me a latex presentation for the sprint review, one slide per section",
    );
    let prompt = stub.prompt_of(0, &parts.vocab);
    assert!(
        prompt.contains(OFFER_MARK) && prompt.contains(SLIDES),
        "the prompt about presentations was handed no offer of `{SLIDES}`: {prompt}"
    );
    // A name and a verb, never the body: the model loads it or it does not.
    assert!(
        !prompt.contains("one idea to a frame"),
        "the offer is not a load: the body came with it: {prompt}"
    );
    assert_eq!(stub.turns(), 1, "one round, one request");
}

/// The prompt says nothing about slides; the model's own reasoning does. The offer composed for
/// the round that reasoning lands in names the skill, which is the case a prompt-only score misses.
#[test]
fn a_skill_the_models_reasoning_names_is_offered_on_the_next_round() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-thought");
    let store = TempDir::new("store-thought");
    skills_in(ws.path());
    let stub = Stub::answering();
    let cfg = config(
        &store.path().join("sessions.db"),
        ws.path(),
        "s-offer-thought",
        stub.endpoint.clone(),
    );
    let parts = parts_for(&cfg);
    stub.script(vec![
        a_tool_call_turn(
            &parts.vocab,
            "this wants a latex presentation: beamer slides, one frame per section",
            "no_such_tool",
            30,
        ),
        an_answer_turn(&parts.vocab, "done", "the deck is planned", 60),
    ]);
    let (_sessions, _hub) = drive(
        &parts,
        &cfg,
        "s-offer-thought",
        "carry on with the thing we discussed yesterday",
    );
    assert_eq!(stub.turns(), 2, "the tool call gave the turn a boundary");
    let first = stub.prompt_of(0, &parts.vocab);
    assert!(
        !first.contains(OFFER_MARK),
        "the fixture's first round must be silent, or the second proves nothing: {first}"
    );
    let second = stub.prompt_of(1, &parts.vocab);
    assert!(
        second.contains(OFFER_MARK) && second.contains(SLIDES),
        "the reasoning named slides and the next round offered nothing: {second}"
    );
}

/// Nothing in the turn is about any skill, so nothing is offered: the floor is what keeps the line
/// worth reading.
#[test]
fn a_turn_about_nothing_the_corpus_holds_is_offered_nothing() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-silent");
    let store = TempDir::new("store-silent");
    skills_in(ws.path());
    let stub = Stub::answering();
    let cfg = config(
        &store.path().join("sessions.db"),
        ws.path(),
        "s-offer-silent",
        stub.endpoint.clone(),
    );
    let parts = parts_for(&cfg);
    stub.script(vec![an_answer_turn(
        &parts.vocab,
        "sourdough needs a warm room",
        "feed the starter at eight",
        30,
    )]);
    let (_sessions, _hub) = drive(
        &parts,
        &cfg,
        "s-offer-silent",
        "how warm should the kitchen be for a sourdough starter",
    );
    let prompt = stub.prompt_of(0, &parts.vocab);
    assert!(
        !prompt.contains(OFFER_MARK),
        "an unrelated turn was handed an offer: {prompt}"
    );
}

/// The offer is never a row: the transcript holds the person's prompt and the model's answer, and
/// nothing that reads as the harness talking to itself.
#[test]
fn the_offer_is_a_tail_and_never_a_row() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let ws = TempDir::new("ws-row");
    let store = TempDir::new("store-row");
    skills_in(ws.path());
    let stub = Stub::answering();
    let cfg = config(
        &store.path().join("sessions.db"),
        ws.path(),
        "s-offer-row",
        stub.endpoint.clone(),
    );
    let parts = parts_for(&cfg);
    stub.script(vec![an_answer_turn(
        &parts.vocab,
        "a plan",
        "here it is",
        30,
    )]);
    let (sessions, _hub) = drive(
        &parts,
        &cfg,
        "s-offer-row",
        "make me a latex presentation for the sprint review",
    );
    let items = sessions
        .harness_of("s-offer-row")
        .expect("the session is open")
        .items()
        .to_vec();
    let rows: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            letibot_transcript::TranscriptItem::User { parts, .. } => Some(
                parts
                    .iter()
                    .filter_map(|p| match p {
                        letibot_transcript::UserPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert!(
        rows.iter().any(|r| r.contains("latex presentation")),
        "the fixture wrote no prompt row to compare against: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains(OFFER_MARK)),
        "the offer became a row the model can never be rid of: {rows:?}"
    );
}

/// A turn that wedges must fail its own test rather than the suite, so every wait here is bounded
/// by the harness's patience, named where it can be seen.
#[allow(dead_code)]
const PATIENCE: Duration = Duration::from_secs(120);
