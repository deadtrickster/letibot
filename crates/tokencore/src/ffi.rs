//! Raw declarations. Nothing in here is public outside the crate.
//!
//! Only `letibot_vocab_load` and `letibot_llama_init` go through the C shim,
//! because only they touch `llama_model_params` by value. Every other symbol
//! below takes plain pointers and integers, so declaring it here cannot
//! disagree with the header the way a transcribed struct can.

use std::ffi::{c_char, c_int, c_void};

pub type LlamaToken = i32;

#[repr(C)]
pub struct LlamaModel {
    _opaque: [u8; 0],
    _nosend: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
pub struct LlamaVocab {
    _opaque: [u8; 0],
    _nosend: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

// `llama_token_attr`, as of the fork's include/llama.h. We only test membership,
// never round-trip the value, so an added variant upstream is harmless here.
pub const ATTR_CONTROL: u32 = 1 << 3;
pub const ATTR_USER_DEFINED: u32 = 1 << 4;

unsafe extern "C" {
    // --- the shim ---
    pub fn letibot_llama_init(quiet: c_int);
    pub fn letibot_vocab_load(path: *const c_char) -> *mut LlamaModel;

    // --- libllama, layout-free ---
    pub fn llama_model_free(model: *mut LlamaModel);
    pub fn llama_model_get_vocab(model: *const LlamaModel) -> *const LlamaVocab;

    pub fn llama_vocab_n_tokens(vocab: *const LlamaVocab) -> i32;
    pub fn llama_vocab_get_text(vocab: *const LlamaVocab, token: LlamaToken) -> *const c_char;
    pub fn llama_vocab_get_attr(vocab: *const LlamaVocab, token: LlamaToken) -> u32;
    pub fn llama_vocab_is_eog(vocab: *const LlamaVocab, token: LlamaToken) -> bool;
    pub fn llama_vocab_bos(vocab: *const LlamaVocab) -> LlamaToken;
    pub fn llama_vocab_eos(vocab: *const LlamaVocab) -> LlamaToken;
    pub fn llama_vocab_get_add_bos(vocab: *const LlamaVocab) -> bool;
    pub fn llama_vocab_get_add_eos(vocab: *const LlamaVocab) -> bool;

    pub fn llama_tokenize(
        vocab: *const LlamaVocab,
        text: *const c_char,
        text_len: i32,
        tokens: *mut LlamaToken,
        n_tokens_max: i32,
        add_special: bool,
        parse_special: bool,
    ) -> i32;

    pub fn llama_detokenize(
        vocab: *const LlamaVocab,
        tokens: *const LlamaToken,
        n_tokens: i32,
        text: *mut c_char,
        text_len_max: i32,
        remove_special: bool,
        unparse_special: bool,
    ) -> i32;

    pub fn llama_token_to_piece(
        vocab: *const LlamaVocab,
        token: LlamaToken,
        buf: *mut c_char,
        length: i32,
        lstrip: i32,
        special: bool,
    ) -> i32;
}

const _: () = {
    // Silence an unused-import warning if the c_void import ever loses its user.
    let _ = core::mem::size_of::<*const c_void>();
};
