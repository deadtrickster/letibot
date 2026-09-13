//! The seam between the turn engine and whatever turns items into spans.
//!
//! # Why the engine does not know about a renderer
//!
//! T1 settled that rendering is **template-driven**: run the model's own shipped
//! jinja through minijinja (`experiments/minijinja-fidelity/RESULTS.md`, D11). That
//! renderer does not exist yet, and building it is not this strand's work. The
//! working renderer today is `GlmRenderer`, hand-written and verified byte-exact
//! against the training runtime over 139 fixtures.
//!
//! So the engine depends on this trait and on [`RenderSpan`], never on a concrete
//! renderer. When the template-driven one lands it implements the same three
//! methods and the engine does not change. The trait is deliberately shaped so that
//! it *can*:
//!
//! * `render` is the whole prompt — the template with `add_generation_prompt=false`;
//! * `render_incremental` is what to append — the difference between two renders;
//! * `generation_prompt` is the tail — the difference `add_generation_prompt` makes.
//!
//! # The one thing this trait must never gain
//!
//! A method returning `Vec<TokenId>`. `RenderSpan` is text-plus-control-token
//! precisely so a renderer needs no vocab, no FFI and no GPU, and so that a user
//! message containing the literal `<|assistant|>` is *structurally* incapable of
//! becoming a turn boundary. Tokenization belongs to `letibot_tokencore`, on the
//! far side of this seam.

use letibot_dialect::{DialectSpec, RenderSpan, StablePrefix};
use letibot_transcript::TranscriptItem;

pub trait PromptRenderer: Send + Sync {
    /// The model this renderer speaks for. The engine reads guards, stop tokens
    /// and `template_sha` from it; it never reads the template itself.
    fn spec(&self) -> &DialectSpec;

    /// The whole prompt for a transcript, with no generation prompt.
    fn render(&self, prefix: &StablePrefix, items: &[TranscriptItem]) -> Vec<RenderSpan>;

    /// The spans to **append** for `new_items`, given the history already rendered.
    ///
    /// `history` is passed in full rather than as an offset because a renderer's
    /// boundary state — is an assistant turn open, was `<think>` already emitted,
    /// was the previous item a tool result — is not derivable from a `usize`. That
    /// was CONTRACT-GAP-1 and it is settled the same way here.
    fn render_incremental(
        &self,
        history: &[TranscriptItem],
        new_items: &[TranscriptItem],
    ) -> Vec<RenderSpan>;

    /// What the model is handed to start speaking, e.g. `<|assistant|><think>`.
    ///
    /// **Not** part of a render: folding it in would break
    /// `render_incremental ≡ render` for any conversation whose next item is a user
    /// message. The engine submits it as an uncommitted tail and commits it as the
    /// leading tokens of the first item the turn produces.
    fn generation_prompt(&self) -> Vec<RenderSpan>;
}

/// `GlmRenderer` behind the seam.
///
/// Off by default (`--features glm`) so that `cargo build -p letibot-turn` proves
/// the engine has no path into a concrete dialect.
#[cfg(feature = "glm")]
pub mod glm {
    use super::*;
    use letibot_dialect_glm::{GlmRenderer, generation_prompt, glm_spec};

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
