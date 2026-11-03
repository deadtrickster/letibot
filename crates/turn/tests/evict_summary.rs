//! What the summary turn is actually shown: a long fake conversation of finished calls, and a
//! backend that records the items it was handed.
//!
//! The record is not the only thing a compaction costs. The summary turn is a real turn over the
//! span it replaces, so every body in that span is prefill the record will never mention - and a
//! body whose call was repeated, whose file was written over, or whose subagent had answered is
//! not something the record had to be asked about at all.

mod support {
    pub mod canned;
    pub mod chatml;
}

use std::sync::{Mutex, MutexGuard, OnceLock};

use letibot_backend::{BackendCaps, BackendError, Completion, Delta, StreamFlow, TurnRequest};
use letibot_dialect::StablePrefix;
use letibot_tokencore::Vocab;
use letibot_transcript::{Speaker, ToolCall, ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::compaction::Answerer;
use letibot_turn::{NullSink, TurnEngine};

use support::canned::Canned;
use support::chatml::{ChatMlParser, ChatMlRenderer};

/// One body in the fake conversation, in KiB: long enough that the bytes are the point.
const BODY: usize = 8;

fn vocab() -> std::sync::Arc<Vocab> {
    static VOCAB: OnceLock<std::sync::Arc<Vocab>> = OnceLock::new();
    VOCAB
        .get_or_init(|| {
            let p = letibot_tokencore::apparatus::gguf_path();
            assert!(p.is_file(), "no vocabulary GGUF at {}", p.display());
            std::sync::Arc::new(letibot_llama::load(&p).expect("the vocabulary must load"))
        })
        .clone()
}

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn engine() -> TurnEngine {
    // The summary goes to the provider, so the local endpoint is never asked; a server that
    // serves nothing refuses anything that reaches it.
    let idle = Canned::serve(Vec::new(), 0);
    TurnEngine::new(
        vocab(),
        std::sync::Arc::new(ChatMlRenderer::default()),
        std::sync::Arc::new(ChatMlParser),
        idle.endpoint.clone(),
        BackendCaps::OWN_SERVER,
        "canned",
        serde_json::json!({}),
    )
    .expect("the fixture dialect resolves against Qwen's vocabulary")
}

/// A provider that answers with one line and records the items each request carried.
struct Recorder {
    seen: Mutex<Vec<Vec<TranscriptItem>>>,
}

impl Recorder {
    fn new() -> Recorder {
        Recorder {
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The items of the last request the provider was handed.
    fn asked(&self) -> Vec<TranscriptItem> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
            .expect("the provider was never asked for a record")
    }
}

impl letibot_backend::MessagesBackend for Recorder {
    fn caps(&self) -> BackendCaps {
        BackendCaps::METERED_API
    }
    fn name(&self) -> &str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "deepseek-chat"
    }
    fn authority(&self) -> String {
        "api.deepseek.com".into()
    }
    fn complete(
        &self,
        req: &TurnRequest<'_>,
        on_delta: &mut dyn FnMut(&Delta) -> StreamFlow,
    ) -> Result<Completion, BackendError> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.items.to_vec());
        let text = "## Objective\n- evict what the record does not need\n";
        let _ = on_delta(&Delta::Text(text.into()));
        Ok(Completion {
            text: text.into(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish: letibot_backend::Finish::Stop,
            cost: letibot_backend::TurnCost {
                meter: letibot_backend::Meter::Metered,
                prompt_tokens: 100,
                cached_tokens: 0,
                generated_tokens: 12,
                wall_ms: 5,
                micros_usd: Some(3),
            },
            raw_usage: None,
        })
    }
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// A body of [`BODY`] KiB, marked so a test can say whether it reached the summary turn.
fn body(mark: &str) -> String {
    let mut s = format!("{mark}\n");
    while s.len() < BODY * 1024 {
        s.push_str("the quick brown fox jumps over the lazy dog, and again, and again\n");
    }
    s
}

fn call(id: &str, name: &str, arguments: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }],
        truncated: false,
    }
}

fn result(id: &str, name: &str, outcome: ToolOutcome, payload: String) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: id.into(),
        name: name.into(),
        outcome,
        payload,
        edit: None,
        origin: None,
        media: None,
    }
}

fn read(id: &str, path: &str, mark: &str) -> [TranscriptItem; 2] {
    [
        call(id, "read", &format!(r#"{{"path":"{path}"}}"#)),
        result(id, "read", ToolOutcome::Ok, body(mark)),
    ]
}

fn bash(id: &str, command: &str, mark: &str) -> [TranscriptItem; 2] {
    [
        call(id, "bash", &format!(r#"{{"command":"{command}"}}"#)),
        result(id, "bash", ToolOutcome::Ok, body(mark)),
    ]
}

fn task_result(id: &str, child: &str, mark: &str) -> [TranscriptItem; 2] {
    [
        call(id, "task_result", &format!(r#"{{"task":"{child}"}}"#)),
        result(id, "task_result", ToolOutcome::Ok, body(mark)),
    ]
}

/// What the summary turn was shown for one call, cut to something a failure message can print.
fn shown(items: &[TranscriptItem], id: &str) -> String {
    let payload = items
        .iter()
        .find_map(|i| match i {
            TranscriptItem::ToolResult {
                call_id, payload, ..
            } if call_id == id => Some(payload.as_str()),
            _ => None,
        })
        .unwrap_or("<no such call>");
    let head: String = payload.chars().take(48).collect();
    format!("{head:?} ({} byte(s))", payload.len())
}

fn bytes(items: &[TranscriptItem]) -> usize {
    items.iter().map(TranscriptItem::bytes).sum()
}

/// The plan rows the session is working from, as the fold is handed them. `src/b.rs` is named by
/// one of them, so that read is the one the eviction may not touch.
fn plan() -> Vec<String> {
    vec![
        "finish src/b.rs and land it".to_string(),
        "keep the record's sections in order".to_string(),
    ]
}

/// A long conversation where most of the bytes are bodies: two windows of one file, a command run
/// twice, a subagent that answered, a failure whose body is the evidence, a file the plan still
/// names, and the exchange in hand.
fn conversation() -> Vec<TranscriptItem> {
    let mut items = vec![user("read the tree and tell me what is wrong")];
    items.extend(read("c1", "src/a.rs", "FIRST-READ-OF-A"));
    items.extend(read("c2", "src/a.rs", "SECOND-READ-OF-A"));
    items.extend(bash("c3", "cargo test", "FIRST-CARGO-TEST"));
    items.extend(bash("c4", "cargo test", "SECOND-CARGO-TEST"));
    items.extend(task_result("c5", "s-abc", "THE-SUBAGENT-REPORT"));
    items.extend([
        call("c8", "bash", r#"{"command":"cargo build"}"#),
        result(
            "c8",
            "bash",
            ToolOutcome::Failed {
                reason: "exit 101".into(),
            },
            body("THE-BUILD-ERROR"),
        ),
    ]);
    items.push(user("get the subagent's answer and look at b"));
    items.extend(read("c6", "src/b.rs", "READ-OF-B"));
    items.push(user("now write it up"));
    items.extend(read("c7", "src/b.rs", "READ-OF-B-IN-HAND"));
    items
}

/// The summary turn over the whole conversation, with the plan beside it.
fn summarise(
    items: &[TranscriptItem],
    rows: &[String],
    tag: &str,
) -> (Recorder, letibot_turn::Harvest) {
    let mut engine = engine();
    let prefix = StablePrefix {
        system: "be terse".into(),
        tools_json: vec![],
    };
    let recorder = Recorder::new();
    let answerer = Answerer::Provider {
        backend: &recorder,
        system: "be terse",
        tools_json: &[],
    };
    let mut sink = NullSink;
    let harvest = letibot_turn::summarise_first_half(
        &mut engine,
        &prefix,
        &format!("{tag}#scratch"),
        items,
        items.len(),
        rows,
        &mut sink,
        &answerer,
    )
    .expect("the summary turn answers");
    (recorder, harvest)
}

/// The whole point: the summary turn is handed the units it needs and not the bodies that are
/// redundant or addressed elsewhere - and the span it is asked about is smaller for it.
#[test]
fn the_summary_turn_is_shown_the_evicted_span() {
    let _lock = serial();
    let items = conversation();
    let before = bytes(&items);
    let (recorder, harvest) = summarise(&items, &plan(), "evict-the-span");
    assert!(!harvest.summary.is_empty(), "a record came back");
    let asked = recorder.asked();

    // A read of the same file twice: the first body cannot be current.
    assert!(
        !shown(&asked, "c1").contains("FIRST-READ-OF-A"),
        "the superseded read's body reached the summary turn: {}",
        shown(&asked, "c1")
    );
    assert!(
        shown(&asked, "c1").contains("[evicted"),
        "a stub stands where the body was: {}",
        shown(&asked, "c1")
    );
    // A command run twice: the earlier output is not the current state of anything.
    assert!(
        !shown(&asked, "c3").contains("FIRST-CARGO-TEST"),
        "a command's earlier output reached the summary turn: {}",
        shown(&asked, "c3")
    );
    // A subagent that answered: its transcript is its own session, and the stub says which.
    assert!(
        !shown(&asked, "c5").contains("THE-SUBAGENT-REPORT"),
        "a settled subagent's report reached the summary turn: {}",
        shown(&asked, "c5")
    );
    assert!(
        shown(&asked, "c5").contains("s-abc"),
        "the stub addresses the child's own session: {}",
        shown(&asked, "c5")
    );
    // A body nothing superseded goes too, once the classes that lose nothing have gone: the call,
    // its outcome and its arguments are what the record needs.
    assert!(
        !shown(&asked, "c2").contains("SECOND-READ-OF-A"),
        "a finished call's body reached the summary turn: {}",
        shown(&asked, "c2")
    );
    // What the plan names is never evicted, even when a later call would supersede it.
    assert!(
        shown(&asked, "c6").contains("READ-OF-B"),
        "a file a plan row names still has its body: {}",
        shown(&asked, "c6")
    );
    // A failure's body is the evidence an error string is: it stays.
    assert!(
        shown(&asked, "c8").contains("THE-BUILD-ERROR"),
        "a failure's body is what the record cannot re-derive: {}",
        shown(&asked, "c8")
    );
    // And the exchange the session is inside keeps everything it has.
    assert!(
        shown(&asked, "c7").contains("READ-OF-B-IN-HAND"),
        "the exchange in hand: {}",
        shown(&asked, "c7")
    );

    // The numbers, off the harvest: what the summariser was never shown.
    assert_eq!(harvest.evicted, 5, "c1, c2, c3, c4 and c5 go");
    assert!(
        harvest.gone >= 5 * (BODY * 1024 - 400),
        "the bodies cost {} byte(s), and only the stubs stayed",
        harvest.gone
    );

    // And the prompt is smaller by most of what went: the summariser's own cost, not only the
    // record's.
    let after = bytes(&asked);
    assert!(
        after + 4 * BODY * 1024 < before,
        "the summary turn was handed {after} byte(s) of a {before}-byte span"
    );
    // The caller's own items are a value, not a buffer.
    assert_eq!(bytes(&items), before);
}

/// A plan that names nothing evicts more: the difference is the plan's, not a coincidence of the
/// conversation's shape.
#[test]
fn an_empty_plan_evicts_what_a_plan_row_would_have_kept() {
    let _lock = serial();
    let items = conversation();
    let (recorder, harvest) = summarise(&items, &[], "evict-the-span-empty");
    let asked = recorder.asked();
    assert!(
        !shown(&asked, "c6").contains("READ-OF-B"),
        "with no plan, the read of b is a finished call like any other: {}",
        shown(&asked, "c6")
    );
    assert_eq!(
        harvest.evicted, 6,
        "the read of b joins them, and the failure and the exchange in hand do not"
    );
    assert!(
        shown(&asked, "c7").contains("READ-OF-B-IN-HAND"),
        "the exchange in hand is still the exception: {}",
        shown(&asked, "c7")
    );
    assert!(
        shown(&asked, "c8").contains("THE-BUILD-ERROR"),
        "and a failure's body is still the evidence: {}",
        shown(&asked, "c8")
    );
}
