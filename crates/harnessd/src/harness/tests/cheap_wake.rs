//! R169: a wake carrying only passing-by room talk runs cheap (thinking off, a small cap) and
//! may end in silence, which is never nudged. Anything addressed makes it a full turn.

use super::super::turns::CHEAP_WAKE_TOKENS;
use super::*;
use letibot_room::chat::Reach;
use std::sync::{Arc, Mutex};

/// What each request asked for, and what the fake answers: `Finish::Length` with only
/// reasoning is the round a nudge would answer on a full turn.
struct Recorder {
    seen: Arc<Mutex<Vec<(Option<bool>, Option<u32>)>>>,
    finish: letibot_backend::Finish,
    reasoning: &'static str,
}

impl letibot_backend::MessagesBackend for Recorder {
    fn caps(&self) -> letibot_backend::BackendCaps {
        letibot_backend::BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "glm"
    }
    fn model(&self) -> &str {
        "glm-5.3"
    }
    fn authority(&self) -> String {
        "a fake provider".into()
    }
    fn complete(
        &self,
        req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        self.seen
            .lock()
            .unwrap()
            .push((req.thinking, req.max_output_tokens));
        Ok(letibot_backend::Completion {
            text: String::new(),
            reasoning: self.reasoning.into(),
            tool_calls: Vec::new(),
            finish: self.finish.clone(),
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 10,
                cached_tokens: 0,
                generated_tokens: 1,
                wall_ms: 1,
                micros_usd: None,
            },
            raw_usage: None,
        })
    }
}

type Seen = Arc<Mutex<Vec<(Option<bool>, Option<u32>)>>>;

fn a_harness(
    id: &str,
    finish: letibot_backend::Finish,
    reasoning: &'static str,
) -> Option<(Harness, Seen, std::path::PathBuf)> {
    let gguf = crate::vocab_gguf()?;
    let dir =
        letibot_sessionlog::testing::scratch_root().join(format!("{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let mut cfg = Config::for_this_box(&dir);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = id.into();
    cfg.vocab_gguf = Some(gguf);
    let parts = Parts::load(&cfg).expect("the vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    let seen: Seen = Arc::default();
    h.provider = Some(Box::new(Recorder {
        seen: seen.clone(),
        finish,
        reasoning,
    }));
    Some((h, seen, dir))
}

fn nudged(h: &Harness) -> bool {
    h.session.items.iter().any(|i| {
        letibot_sessionlog::view::body_of(i).contains("Make the tool call or give the answer now")
    })
}

/// A passing-by note wakes a cheap turn: thinking off and the cap on its request, and an empty
/// answer is the end of it. An addressed note wakes a full one.
#[test]
fn a_passing_by_wake_runs_with_thinking_off_and_the_cap() {
    let Some((mut h, seen, dir)) = a_harness("s-r169-cheap", letibot_backend::Finish::Stop, "")
    else {
        return;
    };
    let me = h.cfg.session_id.clone();
    crate::room::wake::push_chat(
        &me,
        0,
        Reach::Passing,
        "#3 [host] from s-b (session) to all, passing by\n> build is red".into(),
    );
    let reply = h.wake().expect("the wake runs").expect("a turn ran");
    assert_eq!(reply.text, "", "silence is the expected end");
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[(Some(false), Some(CHEAP_WAKE_TOKENS))],
        "one cheap round"
    );
    assert!(!h.cheap_turn, "the flag does not outlive the turn");
    assert_eq!(h.engine.thinking, None);

    crate::room::wake::push_chat(
        &me,
        0,
        Reach::Addressed,
        "#4 [host] from s-b (session) to @you\n> port?".into(),
    );
    let _ = h.wake().expect("the wake runs");
    assert_eq!(
        seen.lock().unwrap().last().copied(),
        Some((None, None)),
        "an addressed note is a full turn"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cheap turn that spends its cap on reasoning ends silently: on a full turn the same round
/// is answered with a nudge and another round.
#[test]
fn a_silent_end_after_a_cheap_wake_is_not_nudged() {
    let Some((mut h, seen, dir)) = a_harness(
        "s-r169-silent",
        letibot_backend::Finish::Length,
        "this is not for me, so",
    ) else {
        return;
    };
    let me = h.cfg.session_id.clone();
    crate::room::wake::push_chat(
        &me,
        0,
        Reach::Passing,
        "#5 [host] from s-b (session) to all, passing by\n> fyi".into(),
    );
    let reply = h
        .wake()
        .expect("a silent end is not an error")
        .expect("a turn ran");
    assert_eq!(reply.text, "");
    assert_eq!(seen.lock().unwrap().len(), 1, "no second round");
    assert!(!nudged(&h), "a cheap turn's silence was nudged");
    let _ = std::fs::remove_dir_all(&dir);
}
