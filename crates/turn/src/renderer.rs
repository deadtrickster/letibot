//! The seam between the turn engine and whatever turns items into spans.
//!
//! The engine depends on this trait and on [`RenderSpan`], never on a concrete renderer: when
//! the template-driven one lands it implements the same three methods and nothing here changes.
//!
//! The one thing this trait must never gain is a method returning `Vec<TokenId>`: `RenderSpan`
//! is text-plus-control-token so a renderer needs no vocab, and a user message containing the
//! literal `<|assistant|>` is structurally incapable of becoming a boundary.

use letibot_dialect::{DialectSpec, RenderSpan, StablePrefix};
use letibot_transcript::TranscriptItem;

pub trait PromptRenderer: Send + Sync {
    /// The model this renderer speaks for. The engine reads guards, stop tokens
    /// and `template_sha` from it; it never reads the template itself.
    fn spec(&self) -> &DialectSpec;

    /// The whole prompt for a transcript, with no generation prompt.
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan>;

    /// The spans to append for `new_items`, given the history already rendered. `history` is
    /// passed in full because a renderer's boundary state - is an assistant turn open, was
    /// `<think>` emitted - is not derivable from a `usize`.
    fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan>;

    /// What the model is handed to start speaking, e.g. `<|assistant|><think>`. Not part of a
    /// render: folding it in would break `render_incremental ≡ render` for a conversation whose
    /// next item is a user message. The engine submits it as an uncommitted tail.
    fn generation_prompt(&self) -> Vec<RenderSpan>;

    /// The same, with the reasoning block already closed. Every dialect here opens one, which is
    /// right for work and wrong for the one turn that is not work: a summary that spends the
    /// window's last tokens thinking produces no record. The default is `generation_prompt`.
    fn generation_prompt_closing_reasoning(&self) -> Vec<RenderSpan> {
        self.generation_prompt()
    }
}

/// `GlmRenderer` behind the seam. Off by default (`--features glm`) so that
/// `cargo build -p letibot-turn` proves the engine has no path into a concrete dialect.
#[cfg(feature = "glm")]
pub mod glm {
    use super::*;
    use letibot_dialect_glm::{
        GlmRenderer, generation_prompt, generation_prompt_closing_reasoning, glm_spec,
    };

    pub struct GlmPromptRenderer {
        renderer: GlmRenderer,
        spec: DialectSpec,
    }

    impl GlmPromptRenderer {
        pub fn new(renderer: GlmRenderer) -> Self {
            GlmPromptRenderer {
                renderer,
                spec: glm_spec(),
            }
        }
    }

    impl Default for GlmPromptRenderer {
        fn default() -> Self {
            GlmPromptRenderer::new(GlmRenderer::new())
        }
    }

    impl PromptRenderer for GlmPromptRenderer {
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
            generation_prompt()
        }
        fn generation_prompt_closing_reasoning(&self) -> Vec<RenderSpan> {
            generation_prompt_closing_reasoning()
        }
    }
}

#[cfg(all(test, feature = "glm"))]
mod tests {
    use super::*;
    use letibot_dialect::spans_to_string;
    use letibot_transcript::UserPart;

    #[test]
    fn the_glm_adapter_agrees_with_the_renderer_it_wraps() {
        let r = glm::GlmPromptRenderer::default();
        let prefix = StablePrefix {
            system: "be terse".into(),
            tools_json: vec![],
        };
        let items = vec![TranscriptItem::User {
            speaker: Default::default(),
            parts: vec![UserPart::Text {
                text: "hello".into(),
            }],
        }];
        let whole = spans_to_string(&r.render(&prefix, &items));
        let head = spans_to_string(&r.render(&prefix, &[]));
        let tail = spans_to_string(&r.render_incremental(&[], &items));
        assert_eq!(whole, format!("{head}{tail}"));
        assert_eq!(
            spans_to_string(&r.generation_prompt()),
            "<|assistant|><think>"
        );
    }
}
