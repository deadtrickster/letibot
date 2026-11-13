//! Kimi's coding plan, for real: `KIMI_LIVE=1 cargo test -p letibot-provider --test kimi_live
//! -- --ignored --nocapture`. One tool-call round trip per door on the global host, on the key
//! `providers.toml` holds under `[kimi-code-global]`. Asserts structure, never content.

use letibot_backend::{BackendError, Completion, MessagesBackend, StreamFlow, TurnRequest};
use letibot_provider::anthropic::AnthropicProvider;
use letibot_provider::keys::{self, Credentials};
use letibot_provider::openai::OpenAiProvider;
use letibot_provider::presets::{KIMI_CODE_GLOBAL, KIMI_CODING};
use letibot_transcript::{ToolOutcome, TranscriptItem, UserPart};

const MODEL: &str = "k3.1-pro";

const TOOLS: &[&str] = &[
    r#"{"type":"function","function":{"name":"echo","description":"Echo the text back.","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}}"#,
];

/// The global plan's key, or `None` with a loud skip when the run was not asked for.
fn creds() -> Option<Credentials> {
    if std::env::var("KIMI_LIVE").ok().as_deref() != Some("1") {
        eprintln!("SKIPPED: KIMI_LIVE is not 1 - the check did not run and this is not a pass");
        return None;
    }
    let mut c = keys::resolve(&KIMI_CODE_GLOBAL, None, None).expect("a key for kimi-code-global");
    // The preset's own URL on both doors: the Messages door shares the global key.
    c.url = None;
    Some(c)
}

fn ask(p: &dyn MessagesBackend, items: &[TranscriptItem], thinking: Option<bool>) -> Completion {
    let tools: Vec<String> = TOOLS.iter().map(|t| t.to_string()).collect();
    let req = TurnRequest {
        system: "You are a test harness. Follow the user exactly.",
        tools_json: &tools,
        items,
        tail: &[],
        max_output_tokens: Some(2048),
        thinking,
    };
    match p.complete(&req, &mut |_| StreamFlow::Continue) {
        Ok(c) => c,
        Err(BackendError::Refused { status, body, .. }) => {
            panic!("{} refused: http {status}: {body}", p.name())
        }
        Err(e) => panic!("{} failed: {e}", p.name()),
    }
}

/// First a tool call, then its result and the answer. `replay` keeps the model's own thought
/// on the second request; without it the history carries none.
fn round_trip(p: &dyn MessagesBackend, thinking: Option<bool>, replay: bool) {
    let mut items = vec![TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text {
            text: "Call the echo tool with text \"ping\". When its result arrives, reply with the \
                   single word done and call nothing else."
                .into(),
        }],
    }];
    let first = ask(p, &items, thinking);
    eprintln!(
        "{} round 1: finish={:?} calls={} thought={}B usage={:?}",
        p.name(),
        first.finish,
        first.tool_calls.len(),
        first.reasoning.len(),
        first.raw_usage
    );
    assert_eq!(first.tool_calls.len(), 1, "one echo call: {:?}", first.text);
    let call = first.tool_calls[0].clone();
    assert_eq!(call.name, "echo");
    if replay {
        for t in &first.thoughts {
            items.push(TranscriptItem::Reasoning {
                text: t.text.clone(),
                field: t.field.clone(),
                truncated: false,
            });
        }
    }
    items.push(TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![call.clone()],
        truncated: false,
    });
    items.push(TranscriptItem::ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        outcome: ToolOutcome::Ok,
        payload: "ping".into(),
        edits: Vec::new(),
        origin: None,
        media: None,
    });
    let second = ask(p, &items, thinking);
    eprintln!(
        "{} round 2 (replay={replay}): finish={:?} text={:?} usage={:?}",
        p.name(),
        second.finish,
        second.text,
        second.raw_usage
    );
    assert!(!second.text.is_empty(), "the second round answered no text");
}

#[test]
#[ignore = "live: calls Kimi's plan - KIMI_LIVE=1 and --ignored"]
fn kimi_code_global_takes_a_tool_round_trip_on_k3_1_pro() {
    let Some(c) = creds() else { return };
    let p = OpenAiProvider::new(&KIMI_CODE_GLOBAL, Some(MODEL), c).with_effort(Some("high"));
    round_trip(&p, Some(true), true);
    round_trip(&p, None, false);
}

#[test]
#[ignore = "live: calls Kimi's plan - KIMI_LIVE=1 and --ignored"]
fn kimi_coding_takes_a_tool_round_trip_on_k3_1_pro() {
    let Some(c) = creds() else { return };
    let mut p = AnthropicProvider::new(&KIMI_CODING, Some(MODEL), c);
    p.sampling = serde_json::json!({"max_tokens": 4096});
    round_trip(&p, Some(true), true);
    round_trip(&p, None, false);
}
