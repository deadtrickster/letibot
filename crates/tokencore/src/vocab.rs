//! The vocabulary, and the two tokenization entry points the dialect contract
//! depends on being different.
//!
//! # The split, and why it is not a convenience
//!
//! `RenderSpan::Text` and `RenderSpan::Control` reach the tokenizer by two
//! functions that cannot produce each other's output:
//!
//! * [`Vocab::tokenize_text`] calls `llama_tokenize` with `parse_special =
//!   false`. A user message containing the literal `<|im_start|>` becomes the
//!   six ordinary tokens `[27, 91, 316, 4747, 91, 29]`, measured. It cannot
//!   become the control id 248045 however it is spelled, because the flag that
//!   would let it is off on this path and there is no argument to turn it on.
//! * [`Vocab::resolve_control`] calls it with `parse_special = true` and then
//!   *rejects anything that is not exactly one id*. It is the only path that
//!   can yield a control id, it takes a `&'static str` literal that came from a
//!   dialect rather than from a conversation, and it runs at startup.
//!
//! That is the structural half of the argument the dialect crate makes in
//! types. A flag argument on one shared `tokenize` would have made the two
//! paths one path with a bool, and the bool is exactly the thing a bug flips.
//!
//! # No implicit specials, ever
//!
//! `add_special` is hard-wired to `false` on both paths. Whether a BOS is
//! prepended is a *dialect* decision expressed as `ControlRole::BeginOfText`,
//! not a property of the GGUF that silently changes when the model file is
//! re-quantized. Letting libllama add one would also break the `/apply-template`
//! diff, which compares our rendered string against the server's -- the server's
//! string has no invisible prefix to compare against.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::sync::Once;

use crate::ffi;

/// A token id. `u32` rather than the FFI's `i32`, to match the type
/// `Dialect::parse` traffics in and because a negative token id is not a value
/// this crate ever wants to be able to represent.
pub type TokenId = u32;

#[derive(Debug)]
pub enum VocabError {
    /// The GGUF could not be opened, or is not a model file libllama recognises.
    Load { path: String },
    /// Text longer than `llama_tokenize` can be told about.
    TextTooLong { bytes: usize },
    /// libllama returned a negative id, or one past the end of the vocab.
    BadTokenId { id: i32 },
    /// A path that is not valid to hand to C.
    BadPath,
    /// `llama_detokenize` produced bytes that are not UTF-8. Only reachable on a
    /// token sequence cut in the middle of a multi-byte character.
    NotUtf8,
}

impl std::fmt::Display for VocabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VocabError::Load { path } => {
                write!(f, "libllama could not load a vocabulary from {path}")
            }
            VocabError::TextTooLong { bytes } => {
                write!(
                    f,
                    "{bytes} bytes of text exceeds llama_tokenize's i32 length"
                )
            }
            VocabError::BadTokenId { id } => write!(f, "token id {id} is out of range"),
            VocabError::BadPath => write!(f, "model path is not representable as a C string"),
            VocabError::NotUtf8 => write!(f, "detokenized bytes are not valid UTF-8"),
        }
    }
}

impl std::error::Error for VocabError {}

static LLAMA_INIT: Once = Once::new();

/// A loaded vocabulary and nothing else.
///
/// Owns a `llama_model *` that was loaded with `vocab_only = true`, so it holds
/// a few megabytes of token table and no weights. Dropping it frees the model.
pub struct Vocab {
    model: *mut ffi::LlamaModel,
    vocab: *const ffi::LlamaVocab,
    n_tokens: u32,
    source: String,
}

// llama.h, above the tokenization block: "The API is thread-safe." Nothing in
// this type mutates the model after construction.
unsafe impl Send for Vocab {}
unsafe impl Sync for Vocab {}

impl Vocab {
    /// Load the vocabulary out of a GGUF.
    ///
    /// For a split model, pass the *first* shard. `vocab_only` returns before
    /// the loader reaches any tensor, so the remaining shards are never opened
    /// and need not exist.
    pub fn load(path: &Path) -> Result<Self, VocabError> {
        LLAMA_INIT.call_once(|| unsafe { ffi::letibot_llama_init(1) });

        let c_path =
            CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| VocabError::BadPath)?;

        let model = unsafe { ffi::letibot_vocab_load(c_path.as_ptr()) };
        if model.is_null() {
            return Err(VocabError::Load {
                path: path.display().to_string(),
            });
        }
        let vocab = unsafe { ffi::llama_model_get_vocab(model) };
        let n = unsafe { ffi::llama_vocab_n_tokens(vocab) };
        Ok(Vocab {
            model,
            vocab,
            n_tokens: n.max(0) as u32,
            source: path.display().to_string(),
        })
    }

    pub fn n_tokens(&self) -> u32 {
        self.n_tokens
    }

    /// The path this vocabulary came from. Recorded in the store so a ledger can
    /// be blamed on a specific GGUF.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn bos(&self) -> Option<TokenId> {
        self.opt_id(unsafe { ffi::llama_vocab_bos(self.vocab) })
    }

    pub fn eos(&self) -> Option<TokenId> {
        self.opt_id(unsafe { ffi::llama_vocab_eos(self.vocab) })
    }

    /// What the GGUF *would* do if we let it. We never do; see the module docs.
    pub fn gguf_would_add_bos(&self) -> bool {
        unsafe { ffi::llama_vocab_get_add_bos(self.vocab) }
    }

    pub fn gguf_would_add_eos(&self) -> bool {
        unsafe { ffi::llama_vocab_get_add_eos(self.vocab) }
    }

    fn opt_id(&self, raw: i32) -> Option<TokenId> {
        if raw < 0 || raw as u32 >= self.n_tokens {
            None
        } else {
            Some(raw as TokenId)
        }
    }

    /// Tokenize a `RenderSpan::Text` payload.
    ///
    /// `parse_special = false`, `add_special = false`. Nothing in `text` can
    /// become a control token.
    pub fn tokenize_text(&self, text: &str) -> Result<Vec<TokenId>, VocabError> {
        self.tokenize_raw(text, false)
    }

    /// Tokenize allowing special-token parsing.
    ///
    /// Private on purpose: the only legitimate caller is
    /// [`Vocab::resolve_control`], which additionally demands a single id. If
    /// this were public somebody would eventually reach for it to tokenize a
    /// rendered prompt in one go, and that is the injection this crate exists
    /// to make impossible.
    fn tokenize_with_specials(&self, text: &str) -> Result<Vec<TokenId>, VocabError> {
        self.tokenize_raw(text, true)
    }

    fn tokenize_raw(&self, text: &str, parse_special: bool) -> Result<Vec<TokenId>, VocabError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let len =
            i32::try_from(text.len()).map_err(|_| VocabError::TextTooLong { bytes: text.len() })?;

        // Every token decodes to at least one byte, so byte length is an upper
        // bound. The negative-return retry below is kept anyway: it is the
        // contract llama_tokenize documents and it costs nothing.
        let mut out: Vec<ffi::LlamaToken> = vec![0; text.len()];
        let mut n = unsafe {
            ffi::llama_tokenize(
                self.vocab,
                text.as_ptr() as *const c_char,
                len,
                out.as_mut_ptr(),
                out.len() as i32,
                false,
                parse_special,
            )
        };
        if n < 0 {
            out.resize((-n) as usize, 0);
            n = unsafe {
                ffi::llama_tokenize(
                    self.vocab,
                    text.as_ptr() as *const c_char,
                    len,
                    out.as_mut_ptr(),
                    out.len() as i32,
                    false,
                    parse_special,
                )
            };
            if n < 0 {
                return Err(VocabError::BadTokenId { id: n });
            }
        }
        out.truncate(n as usize);
        out.into_iter()
            .map(|t| {
                if t < 0 || t as u32 >= self.n_tokens {
                    Err(VocabError::BadTokenId { id: t })
                } else {
                    Ok(t as TokenId)
                }
            })
            .collect()
    }

    /// The exact surface text of one vocabulary entry, unrendered.
    pub fn token_text(&self, id: TokenId) -> Result<&str, VocabError> {
        if id >= self.n_tokens {
            return Err(VocabError::BadTokenId { id: id as i32 });
        }
        let p = unsafe { ffi::llama_vocab_get_text(self.vocab, id as i32) };
        if p.is_null() {
            return Err(VocabError::BadTokenId { id: id as i32 });
        }
        unsafe { CStr::from_ptr(p) }
            .to_str()
            .map_err(|_| VocabError::NotUtf8)
    }

    /// `llama_token_attr` bits for one entry.
    pub fn token_attr(&self, id: TokenId) -> Result<u32, VocabError> {
        if id >= self.n_tokens {
            return Err(VocabError::BadTokenId { id: id as i32 });
        }
        Ok(unsafe { ffi::llama_vocab_get_attr(self.vocab, id as i32) })
    }

    pub fn is_eog(&self, id: TokenId) -> bool {
        id < self.n_tokens && unsafe { ffi::llama_vocab_is_eog(self.vocab, id as i32) }
    }

    /// Render one token to its display piece.
    pub fn piece(&self, id: TokenId, render_special: bool) -> Result<String, VocabError> {
        if id >= self.n_tokens {
            return Err(VocabError::BadTokenId { id: id as i32 });
        }
        let mut buf = vec![0u8; 64];
        let mut n = unsafe {
            ffi::llama_token_to_piece(
                self.vocab,
                id as i32,
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as i32,
                0,
                render_special,
            )
        };
        if n < 0 {
            buf.resize((-n) as usize, 0);
            n = unsafe {
                ffi::llama_token_to_piece(
                    self.vocab,
                    id as i32,
                    buf.as_mut_ptr() as *mut c_char,
                    buf.len() as i32,
                    0,
                    render_special,
                )
            };
            if n < 0 {
                return Err(VocabError::BadTokenId { id: id as i32 });
            }
        }
        buf.truncate(n as usize);
        String::from_utf8(buf).map_err(|_| VocabError::NotUtf8)
    }

    /// Tokens back to text, by concatenating [`Vocab::piece`].
    ///
    /// `render_special = true` reproduces the exact string a dialect rendered,
    /// which is the "ours" side of the `/apply-template` fidelity diff.
    /// `false` reproduces what a reader is meant to see.
    ///
    /// # Why this is not `llama_detokenize`
    ///
    /// **It used to be, and it silently ate spaces before punctuation.** Measured
    /// 2026-09-15 on GLM-5.3-Flash:
    ///
    /// ```text
    /// tokenize("… self.session_id != s …")  ->  [.., 842, 961, 274, ..]
    /// piece(961)              = " !="      <- faithful
    /// llama_detokenize([961]) = "!="       <- space gone
    /// llama_detokenize([842, 961]) = "_id!="   <- gone mid-sequence too
    /// ```
    ///
    /// That is `clean_up_tokenization_spaces`, a HuggingFace post-processing rule
    /// that strips the space before `!`, `?`, `.`, `,` and friends. It is right
    /// for showing prose to a person and catastrophic here, because this function
    /// is how a MODEL'S OWN OUTPUT becomes text: the ids the server streamed are
    /// detokenized to get the assistant's message, tool calls and their arguments
    /// included. So a model that correctly emitted ` !=` had the space removed on
    /// the way out, wrote `old_string: "self.x!= y"` against a file holding
    /// `self.x != y`, and `edit` refused it byte-for-byte — over and over.
    ///
    /// The operator's report is what identified it: *"glm in opencode has zero
    /// problems editing files"*. opencode reads the server's JSON text and never
    /// detokenizes, so it never met this. Being token-native is what exposed us
    /// to it, and `tokenize` was faithful throughout — ` !=` and `!=` are 961 and
    /// 5824, distinct ids — so the model was always SHOWN the right thing. Only
    /// the way back was lossy.
    ///
    /// `piece` per token has neither problem and is what the ledger's own
    /// invariants already compare against.
    pub fn detokenize(
        &self,
        tokens: &[TokenId],
        render_special: bool,
    ) -> Result<String, VocabError> {
        if tokens.is_empty() {
            return Ok(String::new());
        }
        let mut out = String::new();
        for &t in tokens {
            out.push_str(&self.piece(t, render_special)?);
        }
        Ok(out)
    }

    /// Resolve one control literal to its exact single id.
    ///
    /// Three conditions, all of which fail at startup rather than at generation
    /// time:
    ///
    /// 1. the literal must tokenize (with specials parsed) to exactly one id;
    /// 2. that id's vocabulary text must be the literal, byte for byte;
    /// 3. that id must be marked `CONTROL` or `USER_DEFINED`.
    ///
    /// Condition 3 is not decoration and it is not `llama_vocab_is_control`,
    /// which was measured to be *false* for Qwen's `<think>` and `<tool_call>`
    /// (they are `USER_DEFINED`, attr 16, while `<|im_start|>` is `CONTROL`,
    /// attr 8). A check written against `is_control` would have rejected half of
    /// a correct dialect. What condition 3 does reject is a dialect that names
    /// an ordinary word as a boundary: `"hi"` is one id whose text is `"hi"`,
    /// and it passes 1 and 2. A `NORMAL` token as a turn boundary makes the
    /// `Text`/`Control` distinction meaningless for that dialect, so it is a bug
    /// and it should be loud.
    pub fn resolve_control(&self, literal: &str) -> Result<TokenId, ResolveCause> {
        let ids = self
            .tokenize_with_specials(literal)
            .map_err(|_| ResolveCause::Absent)?;
        match ids.as_slice() {
            [] => Err(ResolveCause::Empty),
            [id] => {
                let text = self.token_text(*id).map_err(|_| ResolveCause::Absent)?;
                if text != literal {
                    return Err(ResolveCause::NotSingleToken { ids });
                }
                let attr = self.token_attr(*id).map_err(|_| ResolveCause::Absent)?;
                if attr & (ffi::ATTR_CONTROL | ffi::ATTR_USER_DEFINED) == 0 {
                    return Err(ResolveCause::NotSpecial { id: *id, attr });
                }
                Ok(*id)
            }
            _ => Err(ResolveCause::NotSingleToken { ids }),
        }
    }
}

/// Why one control literal failed to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveCause {
    /// Not in the vocabulary at all.
    Absent,
    /// In the vocabulary as a sequence, not as an entry. `ids` is what it did
    /// produce, so the error can show the caller the damage.
    NotSingleToken { ids: Vec<TokenId> },
    /// A single entry, but a `NORMAL` one -- an ordinary word being declared a
    /// turn boundary.
    NotSpecial { id: TokenId, attr: u32 },
    /// The literal was empty.
    Empty,
}

impl std::fmt::Display for ResolveCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveCause::Absent => write!(f, "absent from the vocabulary"),
            ResolveCause::NotSingleToken { ids } => write!(
                f,
                "not a single vocabulary entry: tokenizes to {} ids {ids:?}",
                ids.len()
            ),
            ResolveCause::NotSpecial { id, attr } => write!(
                f,
                "id {id} is an ordinary token (attr {attr}), not CONTROL or USER_DEFINED"
            ),
            ResolveCause::Empty => write!(f, "the literal is empty"),
        }
    }
}

impl Drop for Vocab {
    fn drop(&mut self) {
        unsafe { ffi::llama_model_free(self.model) };
    }
}
