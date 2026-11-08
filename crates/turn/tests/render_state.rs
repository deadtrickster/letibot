//! The resumable render state `Session` carries: what a series of appends costs, and what a token
//! that no longer describes the transcript does. A byte vocabulary, so this needs no GGUF.

mod support {
    pub mod chatml;
}

use std::sync::Arc;
use std::sync::atomic::Ordering;

use letibot_backend::BackendCaps;
use letibot_dialect::{
    ControlRole, ControlToken, DialectSpec, RenderSpan, RenderState, StablePrefix,
};
use letibot_tokencore::Vocab;
use letibot_transcript::{ReasoningField, ToolCall, ToolOutcome, TranscriptItem, UserPart};
use letibot_turn::events::NullSink;
use letibot_turn::{Endpoint, PromptRenderer, Session, TurnEngine};

use support::chatml::{ChatMlParser, ChatMlRenderer};

fn renderer_spec() -> DialectSpec {
    ChatMlRenderer::default().spec().clone()
}

fn byte_vocab(spec: &DialectSpec) -> Arc<Vocab> {
    let literals: Vec<String> = spec
        .control_tokens
        .iter()
        .map(|t| t.literal.to_string())
        .collect();
    let eog: Vec<String> = spec
        .stop_tokens
        .iter()
        .map(|t| t.literal.to_string())
        .collect();
    Arc::new(Vocab::bytes(
        literals.iter().map(String::as_str),
        eog.iter().map(String::as_str),
    ))
}

fn engine(renderer: ChatMlRenderer) -> TurnEngine {
    let vocab = byte_vocab(&renderer_spec());
    TurnEngine::new(
        vocab,
        Arc::new(renderer),
        Arc::new(ChatMlParser),
        Endpoint::new("127.0.0.1", 1),
        BackendCaps::OWN_SERVER,
        "chatml",
        serde_json::json!({}),
    )
    .expect("the fixture resolves against a byte vocabulary")
}

fn session(engine: &TurnEngine) -> Session {
    engine
        .open(
            "render-state",
            &StablePrefix {
                system: "be terse".into(),
                tools_json: vec![],
            },
        )
        .expect("a session")
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: Default::default(),
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

#[test]
fn the_fixtures_replay_counter_counts_a_replay() {
    // The instrument, before anything is measured with it: the form that is handed the history
    // walks it, and the count is the number of items it walked.
    let r = ChatMlRenderer::default();
    let replays = r.replays();
    let history = [user("one"), user("two"), user("three")];
    let _ = r.render_incremental(&history, &[user("four")]);
    assert_eq!(
        replays.load(Ordering::Relaxed),
        3,
        "the replay went uncounted"
    );
}

/// The defect T10.4 names: one ledger row per item, each rendered against the history, is O(n²)
/// replays. Appending 24 items one at a time must replay nothing, because every call after the
/// first is handed the boundary state instead of the transcript.
#[test]
fn a_series_of_appends_never_replays_the_history() {
    let renderer = ChatMlRenderer::default();
    let replays = renderer.replays();
    let engine = engine(renderer);
    let mut session = session(&engine);
    let mut sink = NullSink;

    let mut items: Vec<TranscriptItem> = Vec::new();
    for n in 0..8 {
        items.push(user(&format!("step {n}")));
        items.push(TranscriptItem::Reasoning {
            text: format!("thinking {n}"),
            field: ReasoningField::Inline,
            truncated: false,
        });
        items.push(TranscriptItem::Assistant {
            text: format!("answer {n}"),
            tool_calls: vec![],
            truncated: false,
        });
    }
    assert_eq!(items.len(), 24);

    for item in &items {
        session
            .append_items(&engine, std::slice::from_ref(item), &mut sink)
            .expect("an append");
    }

    assert_eq!(session.items.len(), items.len());
    let walked = replays.load(Ordering::Relaxed);
    assert_eq!(
        walked, 0,
        "24 single-item appends re-read {walked} item(s) of history; the state token was not carried"
    );
}

/// The same transcript appended in one batch and in twenty-four must build one prompt: the seams
/// a batch is cut at are the item boundaries, whichever call the items arrived in.
#[test]
fn one_batch_and_many_appends_build_the_same_prompt() {
    let engine = engine(ChatMlRenderer::default());
    let mut sink = NullSink;
    let items = vec![
        user("read a"),
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: r#"{"path":"a"}"#.into(),
            }],
            truncated: false,
        },
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: ToolOutcome::Ok,
            payload: "alpha".into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        },
        user("thanks"),
    ];

    let mut whole = session(&engine);
    whole
        .append_items(&engine, &items, &mut sink)
        .expect("one batch");

    let mut split = session(&engine);
    for item in &items {
        split
            .append_items(&engine, std::slice::from_ref(item), &mut sink)
            .expect("an append");
    }

    assert_eq!(split.ledger.tokens(), whole.ledger.tokens());
    assert_eq!(split.ledger.rows().len(), whole.ledger.rows().len());
}

/// A fold rolls items back, and the token taken before the rollback describes a transcript that is
/// no longer this one. Trusting it would leave a turn open that the replay closes, so the length it
/// was taken at is checked and a mismatch replays.
#[test]
fn a_token_taken_before_a_rollback_is_not_trusted() {
    let renderer = ChatMlRenderer::default();
    let replays = renderer.replays();
    let engine = engine(renderer);
    let mut sink = NullSink;
    let items = vec![
        user("one"),
        TranscriptItem::Reasoning {
            text: "think".into(),
            field: ReasoningField::Inline,
            truncated: false,
        },
        TranscriptItem::Assistant {
            text: "two".into(),
            tool_calls: vec![],
            truncated: false,
        },
        user("three"),
    ];

    let mut honest = session(&engine);
    honest
        .append_items(&engine, &items, &mut sink)
        .expect("the honest append");

    // The fold: three items land, one is kept, and the rest arrive again.
    let mut folded = session(&engine);
    folded
        .append_items(&engine, &items[..3], &mut sink)
        .expect("the fold's first half");
    folded.ledger = folded
        .ledger
        .fork("render-state", 1)
        .expect("a fork back to one row");
    folded.items.truncate(1);
    let before = replays.load(Ordering::Relaxed);
    folded
        .append_items(&engine, &items[1..], &mut sink)
        .expect("the rest after the rollback");

    assert!(
        replays.load(Ordering::Relaxed) > before,
        "the token taken before the rollback was trusted"
    );
    assert_eq!(folded.ledger.tokens(), honest.ledger.tokens());
}

/// A batch whose FIRST item fails to tokenize appends nothing, so the transcript is where it was -
/// and the token the batch's render left describes a row that never landed. Keeping it would make
/// the next append start from a closed turn, so a batch's token is only put back once it lands.
#[test]
fn a_batch_that_fails_to_tokenize_leaves_the_token_where_the_transcript_is() {
    let engine = poisoned_engine();
    let mut sink = NullSink;
    let head = [
        user("read a"),
        TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: r#"{"path":"a"}"#.into(),
            }],
            truncated: false,
        },
    ];
    let next = TranscriptItem::Reasoning {
        text: "looking".into(),
        field: ReasoningField::Inline,
        truncated: false,
    };

    // The refusal: the batch opens a user turn, which closes the assistant turn, and then does not
    // land because its first span is a control literal this vocabulary has no id for.
    let mut refused = session(&engine);
    refused
        .append_items(&engine, &head, &mut sink)
        .expect("the conversation before the refusal");
    let err = refused
        .append_items(&engine, &[user("poison")], &mut sink)
        .expect_err("the poisoned item must be refused");
    assert!(err.to_string().contains("not-a-control"), "{err}");
    assert_eq!(refused.items.len(), head.len());
    refused
        .append_items(&engine, std::slice::from_ref(&next), &mut sink)
        .expect("the item after the refusal");

    // A session that never saw the refused item: the assistant turn is still open for both, so the
    // reasoning continues it instead of opening a second one.
    let mut never = session(&engine);
    never
        .append_items(&engine, &head, &mut sink)
        .expect("the conversation");
    never
        .append_items(&engine, &[next], &mut sink)
        .expect("the next item");
    assert_eq!(refused.ledger.tokens(), never.ledger.tokens());
}

/// A renderer that answers a batch containing the poison item with a control literal the
/// vocabulary has no id for, ahead of the item's own spans so the batch's FIRST item fails.
struct Poisoned(ChatMlRenderer);

/// The one item this renderer refuses to let tokenize.
fn is_poison(item: &TranscriptItem) -> bool {
    matches!(item, TranscriptItem::User { parts, .. }
        if parts.iter().any(|p| matches!(p, UserPart::Text { text } if text == "poison")))
}

impl PromptRenderer for Poisoned {
    fn spec(&self) -> &DialectSpec {
        self.0.spec()
    }
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        self.0.render(prefix, items)
    }
    fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan> {
        self.0.render_incremental(history, new_items)
    }
    fn render_incremental_by_item(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        self.0.render_incremental_by_item(history, new_items)
    }
    fn render_incremental_from(
        &self,
        history: &[TranscriptItem],
        state: &mut RenderState,
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        let (mut spans, ends) = self.0.render_incremental_from(history, state, new_items);
        if !new_items.iter().any(is_poison) {
            return (spans, ends);
        }
        spans.insert(
            0,
            RenderSpan::Control(ControlToken::borrowed(
                ControlRole::Other,
                "<|not-a-control|>",
            )),
        );
        (spans, ends.into_iter().map(|e| e + 1).collect())
    }
    fn generation_prompt(&self) -> Vec<RenderSpan> {
        self.0.generation_prompt()
    }
}

fn poisoned_engine() -> TurnEngine {
    let vocab = byte_vocab(&renderer_spec());
    TurnEngine::new(
        vocab,
        Arc::new(Poisoned(ChatMlRenderer::default())),
        Arc::new(ChatMlParser),
        Endpoint::new("127.0.0.1", 1),
        BackendCaps::OWN_SERVER,
        "chatml",
        serde_json::json!({}),
    )
    .expect("the poisoned fixture resolves against a byte vocabulary")
}
