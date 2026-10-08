//! **The GGUF vocabulary**: llama.cpp's tokenizer, as a
//! [`letibot_tokencore::VocabBackend`].
//!
//! [`load`] reads the vocabulary out of a GGUF with `vocab_only = true`, so it holds a few
//! megabytes of token table and no weights — measured at 0.6 s against shard 1 of a
//! six-way split model, the other five shards never opened. The result is an ordinary
//! [`Vocab`]; the rules a vocabulary keeps (one id per control literal, no specials from
//! text, bytes-then-decode) live there, once, for every backend.
//!
//! This is the only crate that links llama.cpp. See `Cargo.toml` for why that moved here.

mod ffi;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::sync::Once;

use letibot_tokencore::{TokenId, Vocab, VocabBackend, VocabError};

static LLAMA_INIT: Once = Once::new();

/// Load the vocabulary out of a GGUF.
///
/// For a split model, pass the *first* shard. `vocab_only` returns before the loader
/// reaches any tensor, so the remaining shards are never opened and need not exist.
pub fn load(path: &Path) -> Result<Vocab, VocabError> {
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
    Ok(Vocab::from_backend(
        Box::new(Gguf {
            model,
            vocab,
            n_tokens: n.max(0) as u32,
        }),
        path.display().to_string(),
    ))
}

/// A `llama_model *` loaded with `vocab_only = true`. Dropping it frees the model.
struct Gguf {
    model: *mut ffi::LlamaModel,
    vocab: *const ffi::LlamaVocab,
    n_tokens: u32,
}

// llama.h, above the tokenization block: "The API is thread-safe." Nothing in
// this type mutates the model after construction.
unsafe impl Send for Gguf {}
unsafe impl Sync for Gguf {}

impl Gguf {
    fn opt_id(&self, raw: i32) -> Option<TokenId> {
        if raw < 0 || raw as u32 >= self.n_tokens {
            None
        } else {
            Some(raw as TokenId)
        }
    }
}

impl VocabBackend for Gguf {
    fn n_tokens(&self) -> u32 {
        self.n_tokens
    }

    fn bos(&self) -> Option<TokenId> {
        self.opt_id(unsafe { ffi::llama_vocab_bos(self.vocab) })
    }

    fn eos(&self) -> Option<TokenId> {
        self.opt_id(unsafe { ffi::llama_vocab_eos(self.vocab) })
    }

    fn gguf_would_add_bos(&self) -> bool {
        unsafe { ffi::llama_vocab_get_add_bos(self.vocab) }
    }

    fn gguf_would_add_eos(&self) -> bool {
        unsafe { ffi::llama_vocab_get_add_eos(self.vocab) }
    }

    fn tokenize(&self, text: &str, parse_special: bool) -> Result<Vec<TokenId>, VocabError> {
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

    fn token_text(&self, id: TokenId) -> Result<&str, VocabError> {
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

    fn token_attr(&self, id: TokenId) -> Result<u32, VocabError> {
        if id >= self.n_tokens {
            return Err(VocabError::BadTokenId { id: id as i32 });
        }
        Ok(unsafe { ffi::llama_vocab_get_attr(self.vocab, id as i32) })
    }

    fn is_eog(&self, id: TokenId) -> bool {
        id < self.n_tokens && unsafe { ffi::llama_vocab_is_eog(self.vocab, id as i32) }
    }

    fn piece_bytes(&self, id: TokenId, render_special: bool) -> Result<Vec<u8>, VocabError> {
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
        Ok(buf)
    }
}

impl Drop for Gguf {
    fn drop(&mut self) {
        unsafe { ffi::llama_model_free(self.model) };
    }
}
