//! Which model this daemon speaks, as a table. Two dialects are wired: GLM-5.3-Flash, whose
//! renderer is 139/139 against the training runtime, and Qwen3.8, whose renderer is 56/56
//! on its own corpus and covers both Qwen targets.

use std::sync::Arc;

use letibot_dialect::{DialectSpec, Parser, RenderSpan, RenderState, StablePrefix};
use letibot_transcript::TranscriptItem;
use letibot_turn::PromptRenderer;
use serde_json::Value;

/// A model this daemon can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `glm-5.3-flash`. Fidelity-gated at 139/139 (`tests/fidelity/run_gate.py`).
    Glm,
    /// `qwen3.8` - Flash-Next and 27B, one byte-identical template.
    /// Checked at 56/56 on its own corpus (`crates/dialect-qwen/fidelity.py`)
    /// with one declared divergence.
    Qwen,
}

impl Dialect {
    pub fn parse(name: &str) -> Option<Dialect> {
        match name {
            "glm" | "glm-5.3-flash" => Some(Dialect::Glm),
            "qwen" | "qwen3.8" | "qwen-3.8-flash-next" | "qwen3.8-27b" => Some(Dialect::Qwen),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dialect::Glm => "glm-5.3-flash",
            Dialect::Qwen => "qwen3.8",
        }
    }

    /// The family name a `[model.<family>]` block is written under, which is the short one
    /// `providers.toml` documents `[model.qwen]` and `[model.glm]`, and [`Dialect::name`]
    /// answers `qwen3.8` - the served name, not the section name.
    pub fn family(self) -> &'static str {
        match self {
            Dialect::Glm => "glm",
            Dialect::Qwen => "qwen",
        }
    }

    /// Everything the engine needs, boxed so the daemon can hold it by value.
    pub fn wiring(self, effort: Option<&str>) -> Wiring {
        match self {
            Dialect::Glm => {
                let effort = effort
                    .and_then(|e| match e {
                        "low" => Some(letibot_dialect_glm::ReasoningEffort::Low),
                        "high" => Some(letibot_dialect_glm::ReasoningEffort::High),
                        "max" | "xhigh" => Some(letibot_dialect_glm::ReasoningEffort::Max),
                        _ => None,
                    })
                    .unwrap_or(letibot_dialect_glm::ReasoningEffort::Max);
                Wiring {
                    dialect: self,
                    renderer: Arc::new(GlmAdapter {
                        renderer: letibot_dialect_glm::GlmRenderer::new().with_effort(effort),
                        spec: letibot_dialect_glm::glm_spec(),
                    }),
                    parser: Arc::new(letibot_dialect_glm::GlmParser),
                }
            }
            Dialect::Qwen => {
                let effort = effort
                    .and_then(letibot_dialect_qwen::ReasoningEffort::parse)
                    .unwrap_or_default();
                Wiring {
                    dialect: self,
                    renderer: Arc::new(QwenAdapter {
                        renderer: letibot_dialect_qwen::QwenRenderer::new().with_effort(effort),
                        spec: letibot_dialect_qwen::qwen_spec(),
                    }),
                    parser: Arc::new(letibot_dialect_qwen::QwenParser),
                }
            }
        }
    }
}

/// A renderer and a parser, owned.
pub struct Wiring {
    pub dialect: Dialect,
    /// `Arc`, not `Box`: an engine CLONES these out, and after the switch to a declared local
    /// model one session's engine can be built from another dialect's wiring than its daemon
    /// opened with.
    pub renderer: Arc<dyn PromptRenderer>,
    pub parser: Arc<dyn Parser>,
}

impl Wiring {
    pub fn spec(&self) -> &DialectSpec {
        self.renderer.spec()
    }

    /// `StablePrefix::tools_json`, from the registry's OpenAI-shaped schemas.
    ///
    /// See the module header for why this is not `registry.tools_json`.
    pub fn tools_json(&self, schemas: &[letibot_tools::ToolSchema]) -> Vec<String> {
        schemas
            .iter()
            .map(|s| {
                let v: Value = serde_json::from_str(&s.prompt_json())
                    .expect("a schema built from Values round-trips");
                match self.dialect {
                    Dialect::Glm => letibot_dialect_glm::glm_tool_json(&v),
                    Dialect::Qwen => letibot_dialect_qwen::qwen_tool_json(&v),
                }
            })
            .collect()
    }

    /// The system update, in the form this dialect can actually render. GLM takes a real
    /// `System { origin: Update }` item; Qwen's template raises on a mid-history system
    /// message, so it takes a user turn with a fixed envelope.
    pub fn system_update(&self, seq: u64, text: &str) -> TranscriptItem {
        match self.dialect {
            Dialect::Glm => TranscriptItem::System {
                text: text.to_string(),
                origin: letibot_transcript::SystemOrigin::Update,
            },
            Dialect::Qwen => letibot_dialect_qwen::system_update_item(seq, text),
        }
    }
}

struct GlmAdapter {
    renderer: letibot_dialect_glm::GlmRenderer,
    spec: DialectSpec,
}

impl PromptRenderer for GlmAdapter {
    fn spec(&self) -> &DialectSpec {
        &self.spec
    }
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        self.renderer.render(prefix, items)
    }
    fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan> {
        self.renderer.render_incremental(history, new_items)
    }
    /// GLM merges nothing, so one pass over the batch cuts at exactly the item seams - which is
    /// what the trait's default, one replay per item, buys at a quadratic price.
    fn render_incremental_by_item(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        self.renderer.render_incremental_by_item(history, new_items)
    }
    fn render_incremental_from(
        &self,
        history: &[TranscriptItem],
        state: &mut RenderState,
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        self.renderer
            .render_incremental_from(history, state, new_items)
    }
    fn generation_prompt(&self) -> Vec<RenderSpan> {
        letibot_dialect_glm::generation_prompt()
    }
    fn generation_prompt_closing_reasoning(&self) -> Vec<RenderSpan> {
        letibot_dialect_glm::generation_prompt_closing_reasoning()
    }
}

struct QwenAdapter {
    renderer: letibot_dialect_qwen::QwenRenderer,
    spec: DialectSpec,
}

impl PromptRenderer for QwenAdapter {
    fn spec(&self) -> &DialectSpec {
        &self.spec
    }
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan> {
        self.renderer.render(prefix, items)
    }
    fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan> {
        self.renderer.render_incremental(history, new_items)
    }
    /// The override that matters: a run of consecutive tool results is ONE user turn and only the
    /// batch knows where the run ends. See `QwenRenderer::render_incremental_by_item`.
    fn render_incremental_by_item(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        self.renderer.render_incremental_by_item(history, new_items)
    }
    fn render_incremental_from(
        &self,
        history: &[TranscriptItem],
        state: &mut RenderState,
        new_items: &[TranscriptItem],
    ) -> (Vec<RenderSpan>, Vec<usize>) {
        self.renderer
            .render_incremental_from(history, state, new_items)
    }
    fn generation_prompt(&self) -> Vec<RenderSpan> {
        letibot_dialect_qwen::generation_prompt()
    }
    fn generation_prompt_closing_reasoning(&self) -> Vec<RenderSpan> {
        letibot_dialect_qwen::generation_prompt_closing_reasoning()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_dialect::spans_to_string;

    #[test]
    fn both_dialects_wire_and_agree_with_themselves() {
        for d in [Dialect::Glm, Dialect::Qwen] {
            let w = d.wiring(None);
            let prefix = StablePrefix {
                system: "be terse".into(),
                tools_json: vec![],
            };
            let items = vec![TranscriptItem::User {
                speaker: Default::default(),
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "hello".into(),
                }],
            }];
            let whole = spans_to_string(&w.renderer.render(&prefix, &items));
            let head = spans_to_string(&w.renderer.render(&prefix, &[]));
            let tail = spans_to_string(&w.renderer.render_incremental(&[], &items));
            assert_eq!(whole, format!("{head}{tail}"), "{}", d.name());
            assert!(!spans_to_string(&w.renderer.generation_prompt()).is_empty());
        }
    }

    #[test]
    fn the_registry_bytes_are_not_the_prompt_bytes() {
        // The seam this module exists to close. If this ever starts passing as equality
        // `Registry::tools_json` became prompt-ready and this indirection can go - but until
        // then, using it directly is a cold prefill on every turn.
        let reg = letibot_tools::read_only_tools(std::sync::Arc::new(
            letibot_tools::builtins::retrieval::Unavailable,
        ))
        .unwrap();
        let raw = reg.tools_json();
        for d in [Dialect::Glm, Dialect::Qwen] {
            let ours = d.wiring(None).tools_json(&reg.schemas());
            assert_eq!(ours.len(), raw.len());
            assert_ne!(
                ours[0],
                raw[0],
                "{}: the registry's separators already match the template's",
                d.name()
            );
            assert!(ours[0].contains(r#""name": "read""#), "{}", ours[0]);
        }
    }

    /// `ToolRuntime::transcript_item` already renders the `NO_RESULT` envelope into
    /// `payload`, so a renderer that wrapped it again would hand the model two. Both dialects
    /// emit `payload` verbatim, and the marker appears once in what they emit.
    #[test]
    fn a_tool_result_is_wrapped_exactly_once() {
        let payload = "NO_RESULT\noutcome: abstained\nreason: nothing to read";
        let item = TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Abstained {
                reason: "nothing to read".into(),
            },
            payload: payload.into(),
            edits: Vec::new(),
            origin: None,
            media: None,
        };
        let prefix = StablePrefix {
            system: "be terse".into(),
            tools_json: vec![],
        };
        for d in [Dialect::Glm, Dialect::Qwen] {
            let got = spans_to_string(
                &d.wiring(None)
                    .renderer
                    .render(&prefix, std::slice::from_ref(&item)),
            );
            assert_eq!(
                got.matches("NO_RESULT").count(),
                1,
                "{}: the envelope is rendered twice, so the model reads two: {got}",
                d.name()
            );
            assert!(got.contains(payload), "{}: {got}", d.name());
        }
    }

    /// The same fact on the pair that matters: the payload comes from `ToolRuntime`, not from a
    /// fixture, and it is the model's bytes whole and once. A renderer that wrapped it again
    /// would put one envelope around another.
    #[test]
    fn the_model_reads_the_runtime_s_payload_once() {
        let result = letibot_tools::ToolResult::new(
            "call-7",
            "bash",
            letibot_transcript::ToolOutcome::Failed {
                reason: "exit 2".into(),
            },
        )
        .with_payload("ls: /nope: No such file or directory");
        let item = letibot_tools::ToolRuntime::transcript_item(&result);
        let TranscriptItem::ToolResult { payload, .. } = &item else {
            unreachable!("transcript_item builds a tool result")
        };
        let prefix = StablePrefix {
            system: "be terse".into(),
            tools_json: vec![],
        };
        for d in [Dialect::Glm, Dialect::Qwen] {
            let got = spans_to_string(
                &d.wiring(None)
                    .renderer
                    .render(&prefix, std::slice::from_ref(&item)),
            );
            assert_eq!(
                got.matches(payload.as_str()).count(),
                1,
                "{}: the runtime's payload is not the model's bytes once: {got}",
                d.name()
            );
            assert!(
                !got.contains("NO_RESULT"),
                "{}: a second envelope was wrapped around the payload: {got}",
                d.name()
            );
        }
    }

    #[test]
    fn a_system_update_is_renderable_on_both() {
        // On Qwen a `System` item past position 0 makes the shipped template raise.
        // The daemon must never build one, and this is where that is decided.
        let qwen = Dialect::Qwen.wiring(None);
        assert!(matches!(
            qwen.system_update(1, "now in Russian"),
            TranscriptItem::User { .. }
        ));
        let glm = Dialect::Glm.wiring(None);
        assert!(matches!(
            glm.system_update(1, "now in Russian"),
            TranscriptItem::System { .. }
        ));
    }

    /// `Session::append_items` seam: the ledger keeps one row per tool result, while the prompt those rows build is the template's one user turn for the run.
    /// A byte vocabulary makes the check exact: every id decodes to its tokenized byte, so the ledger text compares against the template's bytes.
    #[test]
    fn the_ledger_keeps_a_row_per_result_under_one_user_turn() {
        use letibot_tokencore::Vocab;
        use letibot_turn::events::NullSink;

        let w = Dialect::Qwen.wiring(None);
        let spec = w.spec().clone();
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
        let vocab = std::sync::Arc::new(Vocab::bytes(
            literals.iter().map(String::as_str),
            eog.iter().map(String::as_str),
        ));
        let engine = letibot_turn::TurnEngine::new(
            vocab.clone(),
            w.renderer.clone(),
            w.parser.clone(),
            letibot_http::Endpoint::new("127.0.0.1", 1),
            letibot_backend::BackendCaps::OWN_SERVER,
            "qwen",
            serde_json::json!({}),
        )
        .expect("the fixture resolves against a byte vocabulary");
        let prefix = StablePrefix {
            system: "be terse".into(),
            tools_json: vec![],
        };
        let mut session = engine.open("t20", &prefix).expect("a session");
        let mut sink = NullSink;

        // The assistant asks for two reads in one message.
        let ask = TranscriptItem::Assistant {
            text: String::new(),
            tool_calls: vec![
                letibot_transcript::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                },
                letibot_transcript::ToolCall {
                    id: "c2".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"b"}"#.into(),
                },
            ],
            truncated: false,
        };
        session
            .append_items(&engine, std::slice::from_ref(&ask), &mut sink)
            .expect("the ask lands");
        let before_rows = session.ledger.rows().len();
        let before = vocab
            .detokenize(session.ledger.tokens(), true)
            .expect("the fixture's own bytes");

        // Both results arrive in ONE round, which is the batch `append_items` is handed.
        let results: Vec<TranscriptItem> = [("c1", "A"), ("c2", "B")]
            .into_iter()
            .map(|(call_id, payload)| TranscriptItem::ToolResult {
                call_id: call_id.into(),
                name: "read".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: payload.into(),
                edits: Vec::new(),
                origin: None,
                media: None,
            })
            .collect();
        session
            .append_items(&engine, &results, &mut sink)
            .expect("the results land");

        // One row per item...
        assert_eq!(
            session.ledger.rows().len(),
            before_rows + 2,
            "the ledger is append-only and takes one row per item"
        );
        // ...and ONE user turn carrying both results in the prompt.
        let after = vocab
            .detokenize(session.ledger.tokens(), true)
            .expect("the fixture's own bytes");
        let added = after
            .strip_prefix(&before)
            .expect("appending only ever extends the ledger");
        assert_eq!(
            added,
            "<|im_end|>\n<|im_start|>user\n<tool_response>\nA\n</tool_response>\n<tool_response>\nB\n</tool_response><|im_end|>\n",
            "the assistant's turn closes, then the template's one user turn"
        );
        assert_eq!(
            added.matches("<|im_start|>user").count(),
            1,
            "the two results must not open a turn each: {added}"
        );
    }
}
