//! The vocabulary, and the two tokenization entry points the dialect contract needs different:
//! [`Vocab::tokenize_text`] asks with `parse_special = false`, so `<|im_start|>` in a user
//! message becomes ordinary tokens and cannot become a control id. [`Vocab::resolve_control`]
//! parses specials, demands exactly one id, and takes a literal from a dialect, not from a
//! conversation. `add_special` is `false` on both paths - a BOS is a dialect decision, not a
//! property of the GGUF.

//! A [`VocabBackend`] answers [`Vocab`]: a GGUF through llama.cpp, or [`Vocab::bytes`] for a
//! session whose turns go to a cloud provider.

use crate::ledger::hex;

/// A token id. `u32` rather than the FFI's `i32`, to match the type
/// `Dialect::parse` traffics in and because a negative token id is not a value
/// this crate ever wants to be able to represent.
pub type TokenId = u32;

/// `llama_token_attr` bits a control literal must carry one of (see
/// [`Vocab::resolve_control`]). The byte backend marks its reserved ids CONTROL.
pub const ATTR_CONTROL: u32 = 1 << 3;
pub const ATTR_USER_DEFINED: u32 = 1 << 4;

#[derive(Debug)]
pub enum VocabError {
    /// The GGUF could not be opened, or is not a model file libllama recognises.
    Load { path: String },
    /// Text longer than `llama_tokenize` can be told about.
    TextTooLong { bytes: usize },
    /// The backend returned a negative id, or one past the end of the vocab.
    BadTokenId { id: i32 },
    /// A path that is not valid to hand to C.
    BadPath,
    /// Detokenizing produced bytes that are not UTF-8. Only reachable on a
    /// token sequence cut in the middle of a multi-byte character.
    NotUtf8,
    /// A GGUF was asked for and this build has no llama.cpp to read it with.
    NoLlama { path: String },
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
            VocabError::NoLlama { path } => write!(
                f,
                "{path} is a GGUF, and this build has no llama.cpp to read it with (it was \
                 built without the `local` feature). A cloud provider needs no GGUF; a local \
                 model needs a build with `local`"
            ),
        }
    }
}

impl std::error::Error for VocabError {}

/// **What answers a [`Vocab`].** The primitives only; the rules built on them  -
/// one id per control literal, no specials from text, bytes-then-decode - live in
/// [`Vocab`] once, whichever backend is underneath.
pub trait VocabBackend: Send + Sync {
    fn n_tokens(&self) -> u32;
    fn bos(&self) -> Option<TokenId>;
    fn eos(&self) -> Option<TokenId>;
    fn gguf_would_add_bos(&self) -> bool;
    fn gguf_would_add_eos(&self) -> bool;
    /// `add_special` is always false; `parse_special` is the one switch, and
    /// [`Vocab`] sets it to true only inside `resolve_control`.
    fn tokenize(&self, text: &str, parse_special: bool) -> Result<Vec<TokenId>, VocabError>;
    fn token_text(&self, id: TokenId) -> Result<&str, VocabError>;
    fn token_attr(&self, id: TokenId) -> Result<u32, VocabError>;
    fn is_eog(&self, id: TokenId) -> bool;
    fn piece_bytes(&self, id: TokenId, render_special: bool) -> Result<Vec<u8>, VocabError>;
}

/// A loaded vocabulary and nothing else.
pub struct Vocab {
    backend: Box<dyn VocabBackend>,
    source: String,
}

impl Vocab {
    /// A vocabulary answered by `backend`. `source` is what the store records as the
    /// vocabulary a ledger's ids came out of - a GGUF's path, or [`BYTES_SOURCE`] - and
    /// what a resume compares (see [`Vocab::is_bytes`]).
    pub fn from_backend(backend: Box<dyn VocabBackend>, source: impl Into<String>) -> Vocab {
        Vocab {
            backend,
            source: source.into(),
        }
    }

    /// The byte vocabulary, for a session whose model is a cloud provider. `literals` are every
    /// control and stop literal the dialect will ask [`Vocab::resolve_control`] for, each getting
    /// one reserved id in the order given; `eog` are the literals that end generation.
    pub fn bytes<'a>(
        literals: impl IntoIterator<Item = &'a str>,
        eog: impl IntoIterator<Item = &'a str>,
    ) -> Vocab {
        let mut specials: Vec<String> = Vec::new();
        for l in literals {
            if !l.is_empty() && !specials.iter().any(|s| s == l) {
                specials.push(l.to_string());
            }
        }
        let eog: Vec<TokenId> = eog
            .into_iter()
            .filter_map(|l| specials.iter().position(|s| s == l))
            .map(|i| 256 + i as TokenId)
            .collect();
        // The identity is the table, not just the word: two dialects reserve different
        // literals at different ids, so a ledger written under one does not mean the
        // same thing under the other. The hash makes that a string a resume can compare.
        let mut h = <sha2::Sha256 as sha2::Digest>::new();
        for s in &specials {
            sha2::Digest::update(&mut h, s.as_bytes());
            sha2::Digest::update(&mut h, [0u8]);
        }
        let digest: [u8; 32] = sha2::Digest::finalize(h).into();
        let source = format!("{BYTES_SOURCE}:{}", &hex(&digest)[..12]);
        let ascii = (0u8..128).map(char::from).collect();
        Vocab::from_backend(
            Box::new(Bytes {
                specials,
                eog,
                ascii,
            }),
            source,
        )
    }

    /// Whether this is the byte vocabulary - whose ids a llama-server cannot read, so a
    /// session on it must not be pointed at a local model.
    pub fn is_bytes(&self) -> bool {
        self.source.starts_with(BYTES_SOURCE)
    }

    pub fn n_tokens(&self) -> u32 {
        self.backend.n_tokens()
    }

    /// The path this vocabulary came from, or `bytes:<table hash>`. Recorded in the
    /// store so a ledger can be blamed on a specific vocabulary.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn bos(&self) -> Option<TokenId> {
        self.backend.bos()
    }

    pub fn eos(&self) -> Option<TokenId> {
        self.backend.eos()
    }

    /// What the GGUF *would* do if we let it. We never do; see the module docs.
    pub fn gguf_would_add_bos(&self) -> bool {
        self.backend.gguf_would_add_bos()
    }

    pub fn gguf_would_add_eos(&self) -> bool {
        self.backend.gguf_would_add_eos()
    }

    /// Tokenize a `RenderSpan::Text` payload: `parse_special = false`, so nothing in `text` can
    /// become a control token.
    pub fn tokenize_text(&self, text: &str) -> Result<Vec<TokenId>, VocabError> {
        self.backend.tokenize(text, false)
    }

    /// Tokenize allowing special-token parsing. Private on purpose: the only legitimate caller is
    /// [`Vocab::resolve_control`], which additionally demands a single id. Public, it would be
    /// reached for to tokenize a whole rendered prompt, which is the injection this crate prevents.
    fn tokenize_with_specials(&self, text: &str) -> Result<Vec<TokenId>, VocabError> {
        self.backend.tokenize(text, true)
    }

    /// The exact surface text of one vocabulary entry, unrendered.
    pub fn token_text(&self, id: TokenId) -> Result<&str, VocabError> {
        self.backend.token_text(id)
    }

    /// `llama_token_attr` bits for one entry.
    pub fn token_attr(&self, id: TokenId) -> Result<u32, VocabError> {
        self.backend.token_attr(id)
    }

    pub fn is_eog(&self, id: TokenId) -> bool {
        self.backend.is_eog(id)
    }

    /// Render one token to its display piece.
    pub fn piece(&self, id: TokenId, render_special: bool) -> Result<String, VocabError> {
        String::from_utf8(self.piece_bytes(id, render_special)?).map_err(|_| VocabError::NotUtf8)
    }

    /// One token's BYTES, undecoded: a multi-byte character can straddle two tokens, so each half
    /// is invalid UTF-8 on its own and only the pair decodes. Anything assembling a run of tokens
    /// must join the bytes and decode once - see [`Vocab::detokenize`].
    pub fn piece_bytes(&self, id: TokenId, render_special: bool) -> Result<Vec<u8>, VocabError> {
        self.backend.piece_bytes(id, render_special)
    }

    /// Tokens back to text, by concatenating [`Vocab::piece_bytes`]. `render_special = true` reproduces
    /// the exact string a dialect rendered; `false` what a reader is meant to see. It is not
    /// `llama_detokenize`, which ate the space before punctuation through `clean_up_tokenization_spaces`:
    /// ```text
    /// tokenize("… self.session_id != s …")  ->  [.., 842, 961, 274, ..]
    /// piece(961)              = " !="      <- faithful
    /// llama_detokenize([961]) = "!="       <- space gone
    /// llama_detokenize([842, 961]) = "_id!="   <- gone mid-sequence too
    /// ```
    pub fn detokenize(
        &self,
        tokens: &[TokenId],
        render_special: bool,
    ) -> Result<String, VocabError> {
        if tokens.is_empty() {
            return Ok(String::new());
        }
        // Bytes first, decode once: decoding each token on its own makes a character that
        // straddles two tokens invalid UTF-8 in both halves. The space fidelity comes from using
        // `piece` at all rather than `llama_detokenize`, not from decoding one token at a time.
        let mut bytes = Vec::new();
        for &t in tokens {
            bytes.extend_from_slice(&self.piece_bytes(t, render_special)?);
        }
        String::from_utf8(bytes).map_err(|_| VocabError::NotUtf8)
    }

    /// Resolve one control literal to its exact single id: it must tokenize to exactly one id,
    /// that id's vocabulary text must be the literal byte for byte, and that id must be marked
    /// `CONTROL` or `USER_DEFINED` (not `llama_vocab_is_control`, false for Qwen's `<think>`).
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
                if attr & (ATTR_CONTROL | ATTR_USER_DEFINED) == 0 {
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

/// The prefix of the byte vocabulary's [`Vocab::source`].
pub const BYTES_SOURCE: &str = "bytes";

/// **The byte backend.** Ids 0–255 are bytes; `256 + i` is `specials[i]`.
struct Bytes {
    specials: Vec<String>,
    eog: Vec<TokenId>,
    /// Bytes 0..=127 as one string, so `token_text` can lend each as a `&str`.
    ascii: String,
}

impl Bytes {
    fn special(&self, id: TokenId) -> Option<&str> {
        id.checked_sub(256)
            .and_then(|i| self.specials.get(i as usize))
            .map(String::as_str)
    }
}

impl VocabBackend for Bytes {
    fn n_tokens(&self) -> u32 {
        256 + self.specials.len() as u32
    }
    fn bos(&self) -> Option<TokenId> {
        None
    }
    fn eos(&self) -> Option<TokenId> {
        None
    }
    fn gguf_would_add_bos(&self) -> bool {
        false
    }
    fn gguf_would_add_eos(&self) -> bool {
        false
    }
    /// Text is its bytes. With `parse_special` - which only `resolve_control` sets  -
    /// a string that IS a reserved literal, whole, is that literal's one id; anything
    /// else is still bytes, so `"<|im_start|>x"` is not a control token here either.
    fn tokenize(&self, text: &str, parse_special: bool) -> Result<Vec<TokenId>, VocabError> {
        if parse_special && let Some(i) = self.specials.iter().position(|s| s == text) {
            return Ok(vec![256 + i as TokenId]);
        }
        Ok(text.bytes().map(TokenId::from).collect())
    }
    fn token_text(&self, id: TokenId) -> Result<&str, VocabError> {
        if let Some(s) = self.special(id) {
            return Ok(s);
        }
        // A lone byte has no `&str` of its own unless it is ASCII; `ascii` holds every
        // ASCII character once, which is all `token_text` is asked about outside control
        // resolution. A byte above 127 is half a character and has no text.
        match id {
            0..=127 => Ok(&self.ascii[id as usize..id as usize + 1]),
            128..=255 => Err(VocabError::NotUtf8),
            _ => Err(VocabError::BadTokenId { id: id as i32 }),
        }
    }
    fn token_attr(&self, id: TokenId) -> Result<u32, VocabError> {
        match id {
            0..=255 => Ok(0),
            _ if self.special(id).is_some() => Ok(ATTR_CONTROL),
            _ => Err(VocabError::BadTokenId { id: id as i32 }),
        }
    }
    fn is_eog(&self, id: TokenId) -> bool {
        self.eog.contains(&id)
    }
    /// A reserved id renders as its literal only when specials are rendered - the same
    /// contract `llama_token_to_piece` keeps for a control token.
    fn piece_bytes(&self, id: TokenId, render_special: bool) -> Result<Vec<u8>, VocabError> {
        match id {
            0..=255 => Ok(vec![id as u8]),
            _ => match self.special(id) {
                Some(s) if render_special => Ok(s.as_bytes().to_vec()),
                Some(_) => Ok(Vec::new()),
                None => Err(VocabError::BadTokenId { id: id as i32 }),
            },
        }
    }
}

#[cfg(test)]
mod bytes_tests {
    use super::*;
    use crate::control::{VocabDecoder, resolve, resolve_stops, tokenize_spans};
    use letibot_dialect::{
        ControlRole, ControlToken, ControlTokens, RenderSpan, StopToken, TokenDecoder,
    };

    const TOKENS: &[ControlToken] = &[
        ControlToken::borrowed(ControlRole::TurnStartUser, "<|im_start|>user"),
        ControlToken::borrowed(ControlRole::TurnEnd, "<|im_end|>"),
        ControlToken::borrowed(ControlRole::ThinkOpen, "<think>"),
    ];
    const STOPS: &[StopToken] = &[StopToken::borrowed(ControlRole::TurnEnd, "<|im_end|>")];

    fn vocab() -> Vocab {
        Vocab::bytes(
            TOKENS
                .iter()
                .map(|t| t.literal.as_ref())
                .chain(STOPS.iter().map(|t| t.literal.as_ref())),
            STOPS.iter().map(|t| t.literal.as_ref()),
        )
    }

    /// The split the module exists for, on the byte backend: text that spells a control
    /// literal exactly is still its bytes, and only `resolve_control` yields the id.
    #[test]
    fn text_cannot_become_a_control_token_however_it_is_spelled() {
        let v = vocab();
        for text in ["<|im_end|>", "<think>", "a<|im_end|>b", "<|im_start|>user"] {
            let ids = v.tokenize_text(text).unwrap();
            assert_eq!(ids.len(), text.len(), "{text:?} is its bytes");
            assert!(
                ids.iter().all(|id| *id < 256),
                "{text:?} produced a reserved id"
            );
        }
        let end = v.resolve_control("<|im_end|>").unwrap();
        assert!(end >= 256);
        assert_eq!(v.token_attr(end).unwrap(), ATTR_CONTROL);
        assert_eq!(v.token_text(end).unwrap(), "<|im_end|>");
    }

    /// A whole dialect's table resolves - the check that refused every bundled GGUF on a
    /// Mac with no model - and a literal the table never named is refused by name, so a
    /// dialect that grew a token is still a startup error and not a silent byte run.
    #[test]
    fn every_reserved_literal_resolves_and_an_unknown_one_does_not() {
        let v = vocab();
        let map = resolve(&v, &ControlTokens::borrowed(TOKENS)).expect("the table resolves");
        assert_eq!(map.len(), 3);
        let stops = resolve_stops(&v, STOPS).expect("the stops resolve");
        assert!(v.is_eog(stops[0]));
        assert!(matches!(
            v.resolve_control("<tool_call>"),
            Err(ResolveCause::NotSingleToken { .. })
        ));
        assert_eq!(v.resolve_control(""), Err(ResolveCause::Empty));
    }

    /// Bytes first, decode once: a character split across two ids still round-trips, and
    /// a control id renders as its literal only when specials are rendered.
    #[test]
    fn spans_round_trip_and_control_ids_render_only_when_asked() {
        let v = vocab();
        let map = resolve(&v, &ControlTokens::borrowed(TOKENS)).unwrap();
        let spans = [
            RenderSpan::Control(TOKENS[0].clone()),
            RenderSpan::Text("\nпривет, мир — ✓\n".into()),
            RenderSpan::Control(TOKENS[1].clone()),
        ];
        let ids = tokenize_spans(&v, &map, &spans).expect("tokenize");
        assert_eq!(
            v.detokenize(&ids, true).unwrap(),
            "<|im_start|>user\nпривет, мир — ✓\n<|im_end|>"
        );
        assert_eq!(v.detokenize(&ids, false).unwrap(), "\nпривет, мир — ✓\n");
        let dec = VocabDecoder::new(&v, &map);
        assert_eq!(
            dec.control_role(*ids.last().unwrap()),
            Some(ControlRole::TurnEnd)
        );
    }

    /// The identity is the table: the same literals give the same `source`, another
    /// table gives another - which is what a resume compares to refuse replaying ids
    /// written under one meaning against another.
    #[test]
    fn the_source_names_the_table() {
        let a = vocab();
        let b = vocab();
        let c = Vocab::bytes(["<|im_end|>"], []);
        assert!(a.is_bytes() && c.is_bytes());
        assert_eq!(a.source(), b.source());
        assert_ne!(a.source(), c.source());
        assert!(a.source().starts_with("bytes:"));
    }
}
