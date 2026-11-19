//! End to end, on the byte vocabulary: a turn that announced work and stopped is held
//! for the shadow's turn_end verdict, and the verdict decides what the next wake says.

use super::*;
use crate::shadow::{self, Job, runner};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const ANNOUNCED: &str = "I'll run the parser tests next and report back.";

/// The shape measured on db-fuzz's `s-1792687430252160872`: a turn that opens by naming the work
/// it is doing next, with no future tense in it, and stops there.
const ANNOUNCED_IN_PROGRESS: &str =
    "Resuming the two entries. The plpgsql entry I can do properly here";

/// A model that always answers with the same announcement.
struct Says(&'static str);

impl letibot_backend::MessagesBackend for Says {
    fn caps(&self) -> letibot_backend::BackendCaps {
        letibot_backend::BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "fake"
    }
    fn authority(&self) -> String {
        "a fake provider".into()
    }
    fn complete(
        &self,
        _req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        Ok(letibot_backend::Completion {
            text: self.0.into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish: letibot_backend::Finish::Stop,
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 10,
                cached_tokens: 0,
                generated_tokens: 1,
                wall_ms: 1,
                micros_usd: None,
            },
            raw_usage: None,
            thoughts: Vec::new(),
        })
    }
}

/// A shadow model that answers `reply` to every check.
struct Judges(&'static str);

impl shadow::judge::Judge for Judges {
    fn label(&self) -> String {
        "stub/judge".into()
    }
    fn judge(&self, _: &str, _: &str) -> Result<String, String> {
        Ok(self.0.into())
    }
}

#[derive(Default)]
struct Rows(Mutex<Vec<String>>);

impl shadow::record::Recorder for Rows {
    fn row(&self, title: &str, _body: &str) {
        self.0.lock().unwrap().push(title.to_string());
    }
}

/// A harness with the regex prose nudge on and a shadow whose turn_end model answers `reply`, and
/// the channel its wake rings on.
fn a_session(tag: &str, reply: &'static str) -> (Harness, mpsc::Receiver<()>, Arc<Rows>) {
    a_session_saying(tag, ANNOUNCED, Some(reply))
}

/// The same, with the model's own words chosen and the shadow optional: `None` is a box with no
/// shadow job at all, which is the session the model-free half has to carry.
fn a_session_saying(
    tag: &str,
    says: &'static str,
    reply: Option<&'static str>,
) -> (Harness, mpsc::Receiver<()>, Arc<Rows>) {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("shadow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = format!("s-shadow-{tag}");
    at_always_ask(&mut cfg);
    cfg.vocab_gguf = None;
    cfg.intent_prose = true;
    if reply.is_some() {
        cfg.shadow = shadow::config::ShadowConfig::from_lines(
            letibot_provider::keys::ShadowLines {
                enabled: vec!["turn_end".into()],
                ..Default::default()
            },
            "local".into(),
        )
        .unwrap();
    }
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let mut h = Harness::open(&parts, cfg.clone(), Hub::new(&cfg.session_id)).expect("opens");
    h.provider = Some(Box::new(Says(says)));
    let rows = Arc::new(Rows::default());
    let (tx, rx) = mpsc::channel();
    let Some(reply) = reply else {
        return (h, rx, rows);
    };
    let tx = Mutex::new(tx);
    let events: shadow::Events = Default::default();
    let ev = events.clone();
    let on = move |job: Job| -> runner::OnOutcome {
        let ev = ev.clone();
        let tx = tx.lock().unwrap().clone();
        Box::new(move |position, outcome| {
            if job == Job::TurnEnd && *outcome != runner::Outcome::Superseded {
                ev.lock().unwrap().push_back(shadow::Event {
                    job,
                    position,
                    outcome: outcome.clone(),
                });
                let _ = tx.send(());
            }
        })
    };
    let s = runner::Shadow::start(
        &cfg.shadow,
        "deepseek/fake",
        None,
        rows.clone(),
        &|_, _| Arc::new(Judges(reply)),
        &runner::Timing::of,
        &on,
    )
    .expect("turn_end is on");
    h.shadow = shadow::Slot::On {
        shadow: Arc::new(s),
        events,
        recorder: rows.clone(),
        session_id: shadow::session_id(&cfg.session_id),
    };
    (h, rx, rows)
}

fn said(h: &Harness, what: &str) -> bool {
    h.session
        .items
        .iter()
        .any(|i| letibot_sessionlog::view::body_of(i).contains(what))
}

#[test]
fn an_owed_verdict_reminds_with_the_template_and_the_quote() {
    let (mut h, rang, rows) = a_session(
        "owed",
        r#"{"verdict":"owed","promise":"I'll run the parser tests next"}"#,
    );
    h.submit_operator("fix the parser").expect("a turn runs");
    assert!(!said(&h, "[shadow]"), "the turn ends without waiting");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    h.wake()
        .expect("the wake runs")
        .expect("the reminder is a turn");
    assert!(
        said(
            &h,
            "[shadow] You said \"I'll run the parser tests next\" and the turn ended there."
        ),
        "{:?}",
        h.session.items
    );
    let titles = rows.0.lock().unwrap();
    assert!(
        titles
            .iter()
            .any(|t| t.contains("confirms the regex nudge")),
        "{titles:?}"
    );
}

#[test]
fn a_none_verdict_replaces_the_regex_nudge_with_silence() {
    // A persistent shadow's none needs the assistant's or the operator's words as evidence.
    let (mut h, rang, rows) = a_session(
        "none",
        r#"{"verdict":"none","evidence":"I'll run the parser tests next"}"#,
    );
    h.submit_operator("fix the parser").expect("a turn runs");
    let before = h.session.items.len();
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    assert!(h.wake().expect("the wake runs").is_none(), "nothing to say");
    assert_eq!(h.session.items.len(), before, "no nudge row and no turn");
    let titles = rows.0.lock().unwrap();
    assert!(
        titles
            .iter()
            .any(|t| t.contains("replaces the regex nudge")),
        "{titles:?}"
    );
}

#[test]
fn an_unsure_verdict_falls_back_to_the_held_regex_nudge() {
    let (mut h, rang, rows) = a_session("unsure", r#"{"verdict":"unsure"}"#);
    h.submit_operator("fix the parser").expect("a turn runs");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    assert!(
        h.wake().expect("the wake runs").is_some(),
        "the held nudge goes out as a turn"
    );
    assert!(!said(&h, "[shadow] You said"));
    let titles = rows.0.lock().unwrap();
    assert!(
        titles
            .iter()
            .any(|t| t.contains("the regex nudge goes out")),
        "{titles:?}"
    );
}

/// The shadow is published on the watched session's brief - jobs, model, breaker, last verdict -
/// which is what the operator's list and bar draw.
#[test]
fn the_watched_sessions_brief_carries_its_shadow() {
    let (mut h, rang, _rows) = a_session(
        "brief",
        r#"{"verdict":"owed","promise":"I'll run the parser tests next"}"#,
    );
    h.session_registry
        .adopt(
            h.hub.clone(),
            "watched",
            letibot_sessionlog::registry::SessionWiring::default(),
            None,
        )
        .expect("adopted");
    let brief = |h: &Harness| {
        h.session_registry
            .list()
            .into_iter()
            .find(|b| b.session_id == h.cfg.session_id)
            .and_then(|b| b.shadow)
    };
    h.tend_shadow();
    let s = brief(&h).expect("published at the first tend");
    assert_eq!(s.session_id, shadow::session_id(&h.cfg.session_id));
    assert_eq!(s.jobs, vec!["turn_end on".to_string()]);
    assert_eq!(s.model, "stub/judge");
    assert_eq!(s.breaker(), "armed");
    assert_eq!(s.last_verdict, None);
    h.submit_operator("fix the parser").expect("a turn runs");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    let _ = h.wake().expect("the wake runs");
    assert_eq!(
        brief(&h).and_then(|s| s.last_verdict).as_deref(),
        Some("turn_end: owed")
    );
}

/// The db-fuzz shape with the shadow on: a turn that announces in the progressive form is judged
/// by the job, and its owed verdict is what reaches the model.
#[test]
fn an_announcement_in_progress_is_reminded_by_the_shadow() {
    let (mut h, rang, rows) = a_session_saying(
        "progress",
        ANNOUNCED_IN_PROGRESS,
        Some(r#"{"verdict":"owed","promise":"Resuming the two entries."}"#),
    );
    h.submit_operator("carry on with db-fuzz")
        .expect("a turn runs");
    assert!(!said(&h, "[shadow]"), "the turn ends without waiting");
    rang.recv_timeout(Duration::from_secs(10))
        .expect("the verdict rings the wake");
    h.wake()
        .expect("the wake runs")
        .expect("the reminder is a turn");
    assert!(
        said(
            &h,
            "[shadow] You said \"Resuming the two entries.\" and the turn ended there."
        ),
        "{:?}",
        h.session.items
    );
    let titles = rows.0.lock().unwrap();
    assert!(
        titles.iter().any(|t| t.starts_with("turn_end · owed")),
        "{titles:?}"
    );
}

/// And with no shadow at all, the model-free half is what raises it: the same announcement is
/// nudged on the session's own words, which is the path a session with no `[shadow]` takes.
#[test]
fn an_announcement_in_progress_is_nudged_with_no_shadow() {
    let (mut h, _rang, _rows) = a_session_saying("progress-noshadow", ANNOUNCED_IN_PROGRESS, None);
    let reply = h
        .submit_operator("carry on with db-fuzz")
        .expect("a turn runs");
    assert!(
        reply.rounds >= 2,
        "the announcement was not nudged: {reply:?}"
    );
    assert!(
        said(&h, "Resuming the two entries") && said(&h, "do it, or say why not"),
        "{:?}",
        h.session.items
    );
}

/// A shadow's own record takes no turn: opened in a head, it is read, never talked to.
#[test]
fn a_shadow_seat_refuses_a_prompt() {
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("shadow-readonly-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = "s-watched-shadow".into();
    at_always_ask(&mut cfg);
    cfg.vocab_gguf = None;
    cfg.seat = crate::config::Seat::Shadow;
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let mut h = Harness::open(&parts, cfg.clone(), Hub::new(&cfg.session_id)).expect("opens");
    h.provider = Some(Box::new(Says(ANNOUNCED)));
    let err = h.submit_operator("hello").expect_err("refused");
    assert!(err.to_string().contains("read-only"), "{err}");
    assert!(!said(&h, ANNOUNCED), "no model turn ran");
}
