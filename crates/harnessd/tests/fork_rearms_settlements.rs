//! A fork replaces the transcript a settlement's notice was going to be said into, so a
//! settlement the store still has as nobody's news is said into the one it opened - once,
//! however many times the conversation is replaced.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::jobwatch::{BackgroundKind, JobCompletion};
use letibot_harnessd::subagent_record::RecordedSpec;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::registry::{Registry, SessionWiring, Work, WorkOrIdle};
use letibot_tokencore::store::{Store, SubagentRun};
use letibot_tools::builtins::task::TaskSpec;
use letibot_transcript::{TranscriptItem, UserPart};
use letibot_turn::Endpoint;

mod support;

use support::canned;
use support::qwen::a_plain_answer_turn;

const SID: &str = "s-fork-parent";
const CHILD: &str = "s-fork-parent-sub-1";
const ANSWER: &str = "the parser's error paths are fine";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = letibot_sessionlog::testing::scratch(&format!("harnessd-fork-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(store: &std::path::Path, workspace: &std::path::Path, endpoint: Endpoint) -> Config {
    let mut cfg = Config::for_this_box(workspace);
    cfg.dialect = Dialect::Qwen;
    // `leticode` seats the delegation verbs, which is what a child's row was written by.
    cfg.seat = Seat::Leticode;
    cfg.endpoint = endpoint;
    cfg.http_retries = 0;
    cfg.store = Some(store.to_path_buf());
    cfg.session_id = SID.to_string();
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

fn parts_for(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect("the vocabulary must load");
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

fn wiring(cfg: &Config) -> SessionWiring {
    SessionWiring {
        model: cfg.model.clone(),
        dialect: "qwen".into(),
        endpoint: "http://127.0.0.1:1".into(),
        workspace: cfg.workspace.display().to_string(),
    }
}

/// A session in a store of its own, with a daemon's registry - the instance id a settlement's
/// row is stamped with - and `turns` canned answers behind it. `None` where this box cannot run
/// the fixture at all.
struct Fixture {
    harness: Harness,
    registry: Arc<Registry>,
    store: Store,
    /// Dropped after the harness, which is what holds the file open.
    _dir: TempDir,
    _ws: TempDir,
    _model: canned::Canned,
}

impl Fixture {
    fn new(tag: &str, turns: usize) -> Option<Fixture> {
        let _ = letibot_harnessd::vocab_gguf()?;
        let dir = TempDir::new(tag);
        let ws = TempDir::new(&format!("{tag}-ws"));
        let path = dir.0.join("sessions.db");
        // The model first: its endpoint is what the session is opened against.
        let frames_probe = parts_for(&config(&path, &ws.0, Endpoint::new("127.0.0.1", 1)));
        let model = canned::Canned::serve(
            a_plain_answer_turn(&frames_probe.vocab, "thinking", "noted", 30),
            turns,
        );
        let cfg = config(&path, &ws.0, model.endpoint.clone());
        let parts = parts_for(&cfg);
        let registry = Registry::new();
        let hub = registry.new_hub(SID.to_string());
        registry
            .adopt(hub.clone(), SID.to_string(), wiring(&cfg), None)
            .expect("the parent is in the registry");
        let harness = Harness::open_with_registry(&parts, cfg, hub, None, None, registry.clone())
            .expect("the session opens");
        let store = Store::open(&path).expect("the store opens");
        Some(Fixture {
            harness,
            registry,
            store,
            _dir: dir,
            _ws: ws,
            _model: model,
        })
    }

    /// The child's row as a daemon leaves it: finished, and nobody has been told.
    fn a_finished_child(&self) {
        let run = SubagentRun {
            child_id: CHILD.into(),
            parent_id: SID.into(),
            state: "done".into(),
            prompt: "audit the parser's error paths".into(),
            spec_json: RecordedSpec::of(&TaskSpec::default()).to_json(),
            instance: self.registry.instance().to_string(),
            resumes: 0,
            outcome: Some(ANSWER.into()),
            later_answer: None,
            delivered: false,
            started_ms: 1,
            updated_ms: 1,
        };
        self.store.put_subagent_run(&run).expect("the row lands");
    }

    /// The transcript the session is speaking now - what a fork left behind.
    fn now(&self) -> String {
        self.store
            .current_transcript_id(SID)
            .expect("the store is readable")
            .expect("the session has a transcript")
    }

    /// How many settlement bullets of that transcript name `needle` - the bullet and not the
    /// row, because a notice groups several settlements in one row and names each handle twice.
    fn mentions(&self, needle: &str) -> usize {
        self.rows()
            .iter()
            .flat_map(|r| r.lines())
            .filter(|l| l.trim_start().starts_with("- `") && l.contains(needle))
            .count()
    }

    fn rows(&self) -> Vec<String> {
        self.store
            .load_transcript(&self.now())
            .expect("the transcript loads")
            .items
            .into_iter()
            .map(|(item, _, _)| match item {
                TranscriptItem::User { parts, .. } => parts
                    .iter()
                    .map(|p| match p {
                        UserPart::Text { text } => text.clone(),
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                TranscriptItem::Assistant { text, .. } => text,
                TranscriptItem::System { text, .. } => text,
                other => format!("{other:?}"),
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.registry.close();
    }
}

// ---------------------------------------------------------------------------
// THE ONE THIS FILE EXISTS FOR
// ---------------------------------------------------------------------------

/// A settlement that is owed when the conversation is replaced is owed in the NEW one: the
/// fork re-arms it, and the next wake says it there.
#[test]
fn a_settlement_the_store_still_owes_is_said_after_the_fork() {
    let Some(mut f) = Fixture::new("owed", 1) else {
        return;
    };
    f.a_finished_child();
    let fork = f
        .harness
        .clear_conversation(None)
        .expect("the conversation is replaced");
    assert!(
        f.mentions(CHILD) == 0,
        "the fork's new transcript says nothing yet: {:?}",
        f.rows()
    );
    assert!(
        fork.fork.transcript_id.eq(&f.now()),
        "and it is the one the session speaks"
    );
    // A row that has already settled rings for nothing, so the fork itself has to: the turn that
    // says the settlement is the one after this one.
    let rung = f
        .registry
        .next_work_until(Some(std::time::Instant::now() + Duration::from_secs(5)));
    assert!(
        matches!(&rung, WorkOrIdle::Work(Work::Woken(id)) if id == SID),
        "the fork rings for the settlement it re-armed"
    );

    let _ = f.harness.wake().expect("the wake runs");
    assert_eq!(
        f.mentions(CHILD),
        1,
        "the settlement is said once into the new transcript: {:?}",
        f.rows()
    );
    assert!(
        f.store.subagent_run(CHILD).unwrap().unwrap().delivered,
        "and the store is told it has been said"
    );
}

/// The same settlement said twice is worse than said late: what the queue already owes the
/// model is not queued again by the fork.
#[test]
fn a_settlement_the_queue_owes_is_not_said_twice_across_a_fork() {
    let Some(mut f) = Fixture::new("owed-twice", 1) else {
        return;
    };
    f.a_finished_child();
    // The watcher has already queued it: this is the settlement the pen holds, and the store
    // row above is the one its own settle wrote.
    let watch = f.harness.job_watchers().expect("a watcher set");
    watch.queue_for_test(JobCompletion {
        memory: None,
        kind: BackgroundKind::Subagent,
        owner: SID.into(),
        job: CHILD.into(),
        command: String::new(),
        slug: None,
        state: "done".into(),
        produced: 0,
        elapsed_ms: 0,
        detail: ANSWER.into(),
        arrived: std::time::SystemTime::now(),
        later: false,
    });

    let _ = f
        .harness
        .clear_conversation(None)
        .expect("the conversation is replaced");
    let _ = f.harness.wake().expect("the wake runs");
    assert_eq!(f.mentions(CHILD), 1, "one notice, not two: {:?}", f.rows());
}

/// Two forks in a row still owe one notice - the second is not a second re-arm.
#[test]
fn a_second_fork_does_not_re_arm_what_the_first_did() {
    let Some(mut f) = Fixture::new("owed-two-forks", 1) else {
        return;
    };
    f.a_finished_child();
    let _ = f
        .harness
        .clear_conversation(None)
        .expect("the conversation is replaced");
    let _ = f
        .harness
        .clear_conversation(None)
        .expect("the conversation is replaced again");
    let _ = f.harness.wake().expect("the wake runs");
    assert_eq!(
        f.mentions(CHILD),
        1,
        "one notice after two forks: {:?}",
        f.rows()
    );
}
