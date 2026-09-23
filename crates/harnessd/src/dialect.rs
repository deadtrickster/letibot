//! Which model this daemon speaks, as a table.
//!
//! Two dialects are wired: GLM-5.3-Flash, whose renderer is 139/139 against the
//! training runtime, and Qwen3.8, whose renderer is 56/56 on its own corpus and
//! covers both Qwen targets. Selecting one is the *only* place in the daemon that
//! knows a model exists; everything downstream sees `PromptRenderer` and `Parser`.
//!
//! # The seam that did not fit, and what this file does about it
//!
//! `letibot_tools::Registry::tools_json()` produces
//! `serde_json::to_string(schema)`, which uses `,`/`:` with **no spaces**. Both
//! shipped templates render tool schemas through Hugging Face's `tojson`, which is
//! `json.dumps(separators=(", ", ": "))`. So the registry's bytes are *not* the
//! bytes either template emits, and `StablePrefix::tools_json` — which holds
//! finished text — cannot be filled from the registry directly.
//!
//! It is not a rounding error. Every tool schema in the prompt differs, on the
//! first bytes of the stable prefix, which is a cold prefill on every turn of every
//! session. Nothing catches it: the daemon would run, the model would answer, and
//! `f_keep` would sit at whatever the cache happened to give.
//!
//! So [`Wiring::tools_json`] re-serialises through the dialect's own tool-JSON
//! function, and the *dialect* owns those bytes — which is where the rule already
//! lived (`glm_tool_json`'s own doc says so). The registry's `tools_json()` is left
//! alone: it is a reasonable thing for a dialect-free crate to produce, it just is
//! not prompt-ready, and saying that here is cheaper than changing a contract two
//! other strands are built on.

use letibot_dialect::{DialectSpec, Parser, RenderSpan, StablePrefix};
use letibot_transcript::TranscriptItem;
use letibot_turn::PromptRenderer;
use serde_json::Value;

/// A model this daemon can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `glm-5.3-flash`. Fidelity-gated at 139/139 (`tests/fidelity/run_gate.py`).
    Glm,
    /// `qwen3.8` — Flash-Next and 27B, one byte-identical template.
    /// Checked at 56/56 on its own corpus (`crates/dialect-qwen/fidelity.py`),
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
                    renderer: Box::new(GlmAdapter {
                        renderer: letibot_dialect_glm::GlmRenderer::new().with_effort(effort),
                        spec: letibot_dialect_glm::glm_spec(),
                    }),
                    parser: Box::new(letibot_dialect_glm::GlmParser),
                }
            }
            Dialect::Qwen => {
                let effort = effort
                    .and_then(letibot_dialect_qwen::ReasoningEffort::parse)
                    .unwrap_or_default();
                Wiring {
                    dialect: self,
                    renderer: Box::new(QwenAdapter {
                        renderer: letibot_dialect_qwen::QwenRenderer::new().with_effort(effort),
                        spec: letibot_dialect_qwen::qwen_spec(),
                    }),
                    parser: Box::new(letibot_dialect_qwen::QwenParser),
                }
            }
        }
    }
}

/// A renderer and a parser, owned.
pub struct Wiring {
    pub dialect: Dialect,
    pub renderer: Box<dyn PromptRenderer>,
    pub parser: Box<dyn Parser>,
}

impl Wiring {
    pub fn spec(&self) -> &DialectSpec {
        self.renderer.spec()
    }

    /// `StablePrefix::tools_json`, from the registry's OpenAI-shaped schemas.
    ///
    /// See the module header for why this is not `registry.tools_json()`.
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

    /// §5.3's system update, in the form this dialect can actually render.
    ///
    /// GLM takes a real `System { origin: Update }` item; Qwen's template **raises**
    /// on a mid-history system message, so it takes a user turn with a fixed
    /// envelope. The daemon asks for a system change and gets whichever is
    /// renderable, rather than each caller having to know.
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
        // The seam this module exists to close. If this ever starts passing as
        // equality, `Registry::tools_json()` became prompt-ready and this
        // indirection can go — but until then, using it directly is a cold prefill
        // on every turn.
        let reg = letibot_tools::read_only_tools(std::sync::Arc::new(
            letibot_tools::builtins::retrieval::Unavailable,
        ))
        .unwrap();
        let raw = reg.tools_json();
        for d in [Dialect::Glm, Dialect::Qwen] {
            let ours = d.wiring(None).tools_json(&reg.schemas());
            assert_eq!(ours.len(), raw.len());
            assert_ne!(
                ours[0], raw[0],
                "{}: the registry's separators already match the template's",
                d.name()
            );
            assert!(ours[0].contains(r#""name": "read""#), "{}", ours[0]);
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
}
