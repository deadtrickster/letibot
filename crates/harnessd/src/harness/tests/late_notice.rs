//! What the daemon owes a session it is not talking to: `/gate grant` answering a card that had
//! already timed out. The sentence waits in a queue the session drains itself - at the step
//! boundary when a turn runs, as the wake's own row when it does not.

use super::*;

fn a_harness(id: &str) -> (Harness, std::path::PathBuf) {
    let dir =
        letibot_sessionlog::testing::scratch_root().join(format!("{id}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    let mut cfg = Config::for_this_box(&dir);
    at_always_ask(&mut cfg);
    cfg.dialect = crate::Dialect::Qwen;
    cfg.session_id = id.into();
    // No GGUF: a session on a provider answers on the byte vocabulary. This test reads the rows
    // the wake leaves, not any model's words, so it needs no apparatus of its own.
    cfg.vocab_gguf = None;
    cfg.provider = Some(crate::config::ProviderConfig {
        name: "deepseek".into(),
        model: None,
        api_key: Some("sk-not-used".into()),
        thinking: false,
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new(id)).expect("the session opens");
    (h, dir)
}

/// A provider that answers every round with one sentence, so a wake that says something opens a
/// turn the test can read back.
struct Talker;

impl letibot_backend::MessagesBackend for Talker {
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
        _req: &letibot_backend::TurnRequest<'_>,
        _on_delta: &mut dyn FnMut(&letibot_backend::Delta) -> letibot_backend::StreamFlow,
    ) -> Result<letibot_backend::Completion, letibot_backend::BackendError> {
        Ok(letibot_backend::Completion {
            text: "running it again".into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish: letibot_backend::Finish::Stop,
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 10,
                cached_tokens: 0,
                generated_tokens: 3,
                wall_ms: 1,
                micros_usd: None,
            },
            raw_usage: None,
            thoughts: Vec::new(),
        })
    }
}

/// A turn already running reads it at the step boundary: a second turn would answer a sentence
/// the running one has not seen yet, and it could not give the call its answer mid-thought.
#[test]
fn a_running_turn_takes_the_sentence_the_operator_left_for_it() {
    let me = "s-late-boundary";
    let hub = Hub::new(me);
    let note = "[gate] The operator granted your `bash` call, the row `adj-2`";
    letibot_tools::late_grant::queue_notice(me, note.into());
    let mut source = HubSteering::new(hub);
    let took = source.try_next().expect("the boundary takes it");
    assert_eq!(took.text, note);
    assert!(
        !took.from_operator,
        "the operator's grant is the harness's sentence, never their words in the transcript"
    );
    assert!(
        source.try_next().is_none(),
        "taken, not left for the next boundary to say as well"
    );
    assert!(letibot_tools::late_grant::take_notices(me).is_empty());
}

/// And a sentence left for ANOTHER session is not this one's to read: two sessions served by one
/// process share the queue, and the id is what keeps an operator's answer with the call it names.
#[test]
fn a_sentence_left_for_another_session_is_not_this_turns() {
    let other = "s-late-elsewhere";
    let me = "s-late-mine";
    letibot_tools::late_grant::queue_notice(other, "[gate] the row `adj-3`".into());
    let mut source = HubSteering::new(Hub::new(me));
    assert!(source.try_next().is_none(), "not this session's notice");
    assert_eq!(
        letibot_tools::late_grant::take_notices(other).len(),
        1,
        "and still owed to the one it names"
    );
}

/// An idle session is WOKEN for it: the operator's answer is what the work needs, and a queue
/// nobody rings for is the same defect as a card nobody answers.
#[test]
fn a_sentence_left_for_an_idle_session_is_the_first_thing_its_wake_says() {
    let (mut h, dir) = a_harness("s-late-notice");
    h.provider = Some(Box::new(Talker));
    let me = h.cfg.session_id.clone();
    let note = "[gate] The operator granted your `bash` call, the row `adj-1`";
    letibot_tools::late_grant::queue_notice(&me, note.into());

    let reply = h.wake().expect("the wake runs").expect("a turn ran");
    assert_eq!(reply.text, "running it again");
    assert!(
        h.session.items.iter().any(|i| matches!(
            i,
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Agent,
                ..
            } if letibot_sessionlog::view::body_of(i).contains(note)
        )),
        "the model is handed it as the harness's row: {:?}",
        h.session
            .items
            .iter()
            .map(letibot_sessionlog::view::body_of)
            .collect::<Vec<_>>()
    );
    assert!(
        letibot_tools::late_grant::take_notices(&me).is_empty(),
        "and it is said once"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
