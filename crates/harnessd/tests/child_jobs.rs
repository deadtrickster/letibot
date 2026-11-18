//! A child that ends its turn over a job it started has not answered: it is turned back at once,
//! with the job named, and only then does its answer go up - at every placement, and to the bound
//! the plan check keeps.
//!
//! The model is a loopback chat-completions stub on the byte vocabulary, so this file needs no
//! GGUF and no model file; the jobs are a planted list, because this daemon's own process host is
//! a cgroup v2 tree that a test box may not have, and the schedule is the subject here and not the
//! host. One test, because `XDG_CONFIG_HOME` is process-global and it is how the stub is reached.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use letibot_harnessd::config::{Config, ProviderConfig};
use letibot_harnessd::harness::{
    ChildTurns, JOBS_TURN_BACKS, held_said, open_jobs_answer_line, turn_back_for_jobs,
    turned_back_for_its_jobs,
};
use letibot_harnessd::sessions::TodoNagClock;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::SessionEvent;
use letibot_sessionlog::hub::{Hub, OwnWork};
use letibot_sessionlog::registry::Registry;
use letibot_tools::authorise::Speaker;
use letibot_tools::exec::{JobId, JobState, JobView, ScopeId, ScopeKind};
use letibot_transcript::{Speaker as Who, TranscriptItem, UserPart};

mod support;

/// What the child says on each turn it is asked for one: the first is the answer the bug reported,
/// said over a job that was still running; the second is the answer after it waited.
const SAID: [&str; 3] = [
    "All done. Here is the handoff.",
    "The tests pass. Here is the handoff.",
    "It is still running; I am done.",
];

/// The job the child left running: what it is called, what it runs, and how long it has run.
fn a_job() -> JobView {
    let scope = || ScopeId {
        kind: ScopeKind::Session,
        name: "s-child".into(),
        path: std::path::PathBuf::from("/sys/fs/cgroup/x"),
    };
    JobView {
        id: JobId("j41".into()),
        command: "cargo test --release".into(),
        slug: Some("tests".into()),
        scope: scope(),
        owner: scope(),
        cwd: "/".into(),
        pid: 1,
        background: Some(letibot_transcript::Backgrounding::Asked),
        state: JobState::Running,
        elapsed: std::time::Duration::from_secs(4 * 60 + 12),
        ran_for: None,
        produced: 0,
        since_last_output: None,
        memory_cap: letibot_tools::exec::MemoryCap::NotAsked,
        memory: None,
        output_file: None,
        env_report: None,
        holding: None,
        io: None,
        io_marks: Default::default(),
    }
}

/// A loopback chat-completions endpoint that answers the n-th request with `SAID[n]`, and the
/// model listing with none.
fn a_stub() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let url = format!(
        "http://127.0.0.1:{}/v1/chat/completions",
        listener.local_addr().expect("an address").port()
    );
    let rounds = Arc::new(AtomicUsize::new(0));
    let seen = rounds.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let (line, _body) = support::stub::read_request(conn.try_clone().expect("a clone"));
            if !line.starts_with("POST") {
                let _ = write_json(conn, "{\"data\":[]}");
                continue;
            }
            let round = seen.fetch_add(1, Ordering::SeqCst);
            let _ = write_sse(conn, SAID[round.min(SAID.len() - 1)]);
        }
    });
    (url, rounds)
}

fn write_json(mut stream: TcpStream, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// One answer, in the OpenAI streaming shape the provider reads: the text, the finish, a usage
/// count and the end of the stream.
fn write_sse(mut stream: TcpStream, answer: &str) -> std::io::Result<()> {
    let frame = |v: serde_json::Value| format!("data: {v}\n\n");
    let mut body = String::new();
    body.push_str(&frame(
        serde_json::json!({"choices":[{"delta":{"content":answer}}]}),
    ));
    body.push_str(&frame(
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ));
    body.push_str(&frame(serde_json::json!({"choices":[],"usage":{
        "prompt_tokens": 50,
        "completion_tokens": 9,
    }})));
    body.push_str("data: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// The child's own rows of one speaker, as their text - what the model was actually sent.
fn rows_of(hub: &Hub, speaker: Who) -> Vec<String> {
    hub.retained()
        .into_iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TranscriptContent { item, .. }
            | SessionEvent::TranscriptAppended {
                item: Some(item), ..
            } => match &**item {
                TranscriptItem::User { speaker: s, parts } if *s == speaker => Some(
                    parts
                        .iter()
                        .filter_map(|p| match p {
                            UserPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The whole of the rule, measured once: the turn-back, the answer that follows it, and the bound.
#[test]
fn a_child_that_ends_over_a_job_is_turned_back_and_answers_after_it() {
    let scratch = letibot_sessionlog::testing::scratch("letibot-child-jobs");
    std::fs::create_dir_all(scratch.join("letibot")).expect("the scratch config dir");
    // SAFETY: one test in this binary sets these, before any thread it starts reads them; the
    // other tests here read nothing out of the environment.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &scratch);
        std::env::set_var("XDG_DATA_HOME", &scratch);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    let (url, rounds) = a_stub();
    // The operator's own `providers.toml` is not this test's: the endpoint is the stub's.
    std::fs::write(
        scratch.join("letibot/providers.toml"),
        format!(
            "# Written by child_jobs.rs. Nothing here is the operator's file.\n\
             [deepseek]\nkey = \"sk-test\"\nurl = \"{url}\"\n"
        ),
    )
    .expect("the providers file");

    let dir = scratch.join("ws");
    std::fs::create_dir_all(&dir).expect("the workspace");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = Dialect::Qwen;
    cfg.http_retries = 0;
    // The byte vocabulary: a session whose turns go to a provider needs no GGUF.
    cfg.vocab_gguf = None;
    cfg.provider = Some(ProviderConfig {
        name: "deepseek".into(),
        model: Some("deepseek-chat".into()),
        api_key: None,
        thinking: false,
    });
    cfg.context_window = None;
    cfg.store = Some(scratch.join("sessions.db"));
    cfg.session_id = "s-child".into();
    cfg.permission = Vec::new();
    let parts = Parts::load(&cfg).expect("the byte vocabulary needs no model file");
    *parts.mode_store.write().unwrap_or_else(|e| e.into_inner()) =
        letibot_harnessd::modes::ModeStore::default();
    let registry = Registry::new();
    let hub = registry.new_hub("s-child".to_string());
    let mut sub = Harness::open_with_registry(&parts, cfg, hub, None, None, registry)
        .expect("the child opens");

    let mut reply = sub
        .submit_as_a_normal_session(Speaker::Operator, "run the tests and hand off")
        .expect("the first turn answers");
    assert_eq!(
        reply.text, SAID[0],
        "the child's first answer is the one the bug reported: said over a running job"
    );

    // The job ENDS while the child is turned back - the wait it was sent to do.
    let mut turns = ChildTurns::new(&sub, "s-child", TodoNagClock::new());
    let published: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let reads = AtomicUsize::new(0);
    turned_back_for_its_jobs(
        &mut sub,
        &mut turns,
        "the child",
        &mut reply,
        &|_h: &Harness| {
            (reads.fetch_add(1, Ordering::SeqCst) == 0)
                .then(a_job)
                .into_iter()
                .collect()
        },
        &|state, said| {
            published
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((state.to_string(), said.to_string()))
        },
    );

    // The turn-back turn: an agent row naming the job, and the two ways out.
    let sent = rows_of(sub.hub(), Who::Agent);
    let notice = sent
        .iter()
        .find(|t| t.contains("[subagent jobs]"))
        .unwrap_or_else(|| panic!("the child was not turned back; its rows: {sent:#?}"));
    assert!(
        notice.contains("`tests · j41` (`cargo test --release`, 4m12s)"),
        "the notice names the job by handle, command and age: {notice}"
    );
    assert!(
        notice.contains("`job_wait`") && notice.contains("`job_kill`"),
        "and the two ways out of it: {notice}"
    );

    // And the answer that goes up is the one said AFTER it, not the one said over the job.
    assert_eq!(
        reply.text, SAID[1],
        "the answer is the turn after the wait, not the one that was said over the job"
    );
    assert_eq!(
        rounds.load(Ordering::SeqCst),
        2,
        "the child took a second turn - the turn-back - and it was the model's"
    );
    assert!(
        !reply.text.contains("[subagent jobs]"),
        "nothing was left running, so the answer carries no open-job line: {}",
        reply.text
    );

    // The parent's row said so while it happened, and names the same job the notice does.
    let rows = published.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(rows.len(), 1, "one row for one turn-back: {rows:?}");
    assert_eq!(
        rows[0].0, "running",
        "a turned-back child is running a turn"
    );
    assert!(
        rows[0].1.contains("`tests · j41`"),
        "the row names the job: {}",
        rows[0].1
    );

    // The bound: a job that never ends is told ONCE, and the answer then goes up saying so.
    let mut bound_reply = sub
        .submit_as_a_normal_session(Speaker::Operator, "and now?")
        .expect("the third turn answers");
    let mut turns = ChildTurns::new(&sub, "s-child", TodoNagClock::new());
    turned_back_for_its_jobs(
        &mut sub,
        &mut turns,
        "the child",
        &mut bound_reply,
        &|_h: &Harness| vec![a_job()],
        &|_state, _said| {},
    );
    let notices = rows_of(sub.hub(), Who::Agent)
        .iter()
        .filter(|t| t.contains("[subagent jobs]"))
        .count();
    assert_eq!(
        notices, 2,
        "each ending is told once: one notice for the wait, one for the job that never ended \
         ({notices} for two endings)"
    );
    assert_eq!(
        bound_reply.text, SAID[2],
        "the second ending was turned back once and then its answer stood"
    );
    // And what the runner adds to an answer over a job it could not settle: the job, by name.
    let line = open_jobs_answer_line(&[a_job()]).expect("a running job is a line");
    assert!(
        line.contains("`tests · j41` (`cargo test --release`, 4m12s)"),
        "{line}"
    );
    assert_eq!(
        rounds.load(Ordering::SeqCst),
        4,
        "one turn per ending and one turn-back each: two prompts, two turn-backs - a third \
         turn-back on the job that never ends would be a fifth: {}",
        rounds.load(Ordering::SeqCst)
    );

    // A job's exit while the child is turned back wakes it as it always did: the turn-back takes
    // nothing off the child's own queue, so the ring a settlement makes still finds the loop.
    assert!(sub.hub().wake_its_own_reader(), "the ring is taken");
    assert!(
        matches!(sub.hub().take_own_work(), OwnWork::Wake),
        "a wake rung on a turned-back child is the next thing its loop takes"
    );
}

/// The decision itself: nothing running is no notice, one job is one notice, and the same notice
/// twice is not news - the metronome rule the plan check keeps, one subject over.
#[test]
fn a_running_job_is_turned_back_once_and_the_same_job_is_not_said_twice() {
    let mut left = JOBS_TURN_BACKS;
    let mut told = None;
    assert!(
        turn_back_for_jobs(&[], &mut left, &mut told).is_none(),
        "a child with nothing running holds for nothing"
    );
    assert!(turn_back_for_jobs(&[a_job()], &mut left, &mut told).is_some());
    assert_eq!(left, 0, "the budget is one, as the intent check's is");
    assert!(
        turn_back_for_jobs(&[a_job()], &mut left, &mut told).is_none(),
        "a spent budget does not turn the child back again"
    );
    // The metronome half: the same notice is not said twice.
    let mut again = JOBS_TURN_BACKS;
    let mut said = None;
    assert!(turn_back_for_jobs(&[a_job()], &mut again, &mut said).is_some());
    assert!(
        turn_back_for_jobs(&[a_job()], &mut again, &mut said).is_none(),
        "the same notice is not said twice"
    );
}

/// The whole vocabulary of the hold, in one place: what the parent's row says, what the answer
/// says, and what one job is called in both.
#[test]
fn the_held_row_and_the_answer_name_the_job_the_same_way() {
    let jobs = [a_job()];
    assert_eq!(
        held_said(&jobs).unwrap(),
        "answered - held for `tests · j41` (`cargo test --release`, 4m12s)"
    );
    let line = open_jobs_answer_line(&jobs).unwrap();
    assert!(line.starts_with("[subagent jobs] 1 job(s)"), "{line}");
    assert!(
        line.contains("`tests · j41` (`cargo test --release`, 4m12s)"),
        "{line}"
    );
    assert_eq!(held_said(&[]), None, "nothing running is no row");
    assert_eq!(open_jobs_answer_line(&[]), None);
}
