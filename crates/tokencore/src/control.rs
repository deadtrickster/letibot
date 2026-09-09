//! Resolving a dialect's control tokens once, at startup, tokenizing a rendered
//! span sequence, and handing a parser the vocabulary it is not allowed to own.
//!
//! # Why this fails at startup and not at generation time
//!
//! A control literal that is absent from the vocabulary, or that is really a
//! *sequence*, does not produce an error at the point of use -- it produces a
//! prompt in which a turn boundary is spelled out as ordinary text. The model
//! sees no boundary, answers as though the conversation were one long user
//! message, and every downstream number looks fine. The ledger hashes agree
//! with themselves forever, because they are hashing exactly what was sent.
//!
//! So [`resolve`] runs over the *whole* `ControlTokens` set before a single
//! token is appended to a ledger, and it reports *every* failure rather than
//! the first. A dialect with three broken literals should cost one startup, not
//! three.
//!
//! [`resolve_stops`] is the same rule for the other direction. A stop token that
//! silently is not one token never fires and the turn runs to `n_ctx`, so it is
//! resolved at the same moment, against the same vocabulary, and reported with
//! the same machinery -- and since a [`StopToken`] carries its role, the report
//! can say *which boundary* was lost rather than printing a bare string.

use std::collections::HashMap;

use letibot_dialect::{
    ControlRole, ControlToken, ControlTokens, RenderSpan, StopToken, TokenDecoder,
};

use crate::vocab::{ResolveCause, TokenId, Vocab};

/// Every control token of one dialect, resolved to exact ids.
///
/// Constructed only by [`resolve`], so a `ControlMap` in hand is proof that the
/// vocabulary agreed with the dialect at startup.
///
/// Keyed by **literal**, in both directions that matter. A role is a label a
/// dialect chose; a literal is what the template emits and what the vocabulary
/// answers for, and one role may own several of them.
#[derive(Debug, Clone)]
pub struct ControlMap {
    by_literal: HashMap<String, TokenId>,
    /// Every id a role owns, in declaration order. A `Vec`, not a `TokenId`:
    /// `<|user|>` and `<|observation|>` are both ways GLM ends a generation, and
    /// a map that kept one of them would be answering a different question.
    by_role: HashMap<ControlRole, Vec<TokenId>>,
    role_of: HashMap<TokenId, ControlRole>,
}

impl ControlMap {
    /// The id a literal resolves to. **The primary lookup**: a literal is exactly
    /// one vocabulary entry or resolution failed at startup.
    pub fn id(&self, literal: &str) -> Option<TokenId> {
        self.by_literal.get(literal).copied()
    }

    /// Every id carrying a role, in the order the dialect declared them.
    ///
    /// This replaced a `role(role) -> Option<TokenId>` that returned whichever of
    /// them happened to be inserted last into a `HashMap` -- which is to say, an
    /// arbitrary one, silently.
    pub fn ids_for_role(&self, role: ControlRole) -> &[TokenId] {
        self.by_role.get(&role).map_or(&[], Vec::as_slice)
    }

    /// The role of an id, for the parse direction. `None` for ordinary text.
    pub fn role_of(&self, id: TokenId) -> Option<ControlRole> {
        self.role_of.get(&id).copied()
    }

    /// How many distinct literals resolved. Literals, not roles: a dialect with
    /// three spellings of `TurnEnd` resolved three things.
    pub fn len(&self) -> usize {
        self.by_literal.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_literal.is_empty()
    }

    /// Every control id, deduplicated. Two roles may share one id -- several
    /// dialects end a turn and end a message with the same literal -- and that
    /// is not an error.
    pub fn ids(&self) -> Vec<TokenId> {
        let mut v: Vec<TokenId> = self.by_literal.values().copied().collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Whether an id the model generated is one of this dialect's boundaries.
    pub fn is_control_id(&self, id: TokenId) -> bool {
        self.role_of.contains_key(&id)
    }
}

/// One literal that would not resolve.
///
/// `literal` is the dialect's own `Cow`, cloned: a compile-time dialect borrows
/// and allocates nothing even on the failure path, and a dialect loaded from a
/// GGUF can be reported at all -- which under the old `&'static str` it could
/// not be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFailure {
    pub role: ControlRole,
    pub literal: std::borrow::Cow<'static, str>,
    pub cause: ResolveCause,
}

/// Every failure at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlResolveError {
    /// Sorted by `(role, literal)`. Deterministic because `ControlRole` is `Ord`:
    /// two runs against the same broken vocabulary print the same report, so a
    /// diff of two startup logs is about the vocabulary and not about hashing.
    pub failures: Vec<ControlFailure>,
}

impl std::fmt::Display for ControlResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{} control token(s) could not be resolved against the vocabulary:",
            self.failures.len()
        )?;
        for fail in &self.failures {
            writeln!(f, "  {:?} {:?}: {}", fail.role, fail.literal, fail.cause)?;
        }
        Ok(())
    }
}

impl std::error::Error for ControlResolveError {}

fn sorted(mut failures: Vec<ControlFailure>) -> ControlResolveError {
    failures.sort_by(|a, b| a.role.cmp(&b.role).then_with(|| a.literal.cmp(&b.literal)));
    ControlResolveError { failures }
}

/// Resolve a dialect's whole control-token set against a vocabulary.
///
/// Call this once, at startup, before any ledger exists.
pub fn resolve(vocab: &Vocab, tokens: &ControlTokens) -> Result<ControlMap, ControlResolveError> {
    let mut by_literal = HashMap::new();
    let mut by_role: HashMap<ControlRole, Vec<TokenId>> = HashMap::new();
    let mut role_of = HashMap::new();
    let mut failures = Vec::new();

    for token in tokens.iter() {
        match vocab.resolve_control(&token.literal) {
            Ok(id) => {
                by_literal.insert(token.literal.clone().into_owned(), id);
                let ids = by_role.entry(token.role).or_default();
                if !ids.contains(&id) {
                    ids.push(id);
                }
                role_of.entry(id).or_insert(token.role);
            }
            Err(cause) => failures.push(ControlFailure {
                role: token.role,
                literal: token.literal.clone(),
                cause,
            }),
        }
    }

    if failures.is_empty() {
        Ok(ControlMap {
            by_literal,
            by_role,
            role_of,
        })
    } else {
        Err(sorted(failures))
    }
}

/// Resolve a dialect's stop tokens the same way.
///
/// Same rules, same startup timing. A stop token that is silently a sequence
/// never fires, and a turn that never stops is a turn that runs to `n_ctx` --
/// so this is not a convenience, it is the only moment the failure is cheap.
/// The role travels with the literal so the report names the boundary that was
/// lost.
pub fn resolve_stops(
    vocab: &Vocab,
    stops: &[StopToken],
) -> Result<Vec<TokenId>, ControlResolveError> {
    let mut ids = Vec::with_capacity(stops.len());
    let mut failures = Vec::new();
    for stop in stops {
        match vocab.resolve_control(&stop.literal) {
            Ok(id) => ids.push(id),
            Err(cause) => failures.push(ControlFailure {
                role: stop.role,
                literal: stop.literal.clone(),
                cause,
            }),
        }
    }
    if failures.is_empty() {
        Ok(ids)
    } else {
        Err(sorted(failures))
    }
}

/// The real [`TokenDecoder`]: the one implementation that has a vocabulary.
///
/// `letibot-dialect` declares the trait and cannot implement it -- it has no
/// vocab by design -- and a parser needs it to turn ids back into `Content`.
/// This is where the two meet, and it is the only place in the tree that knows
/// both.
pub struct VocabDecoder<'a> {
    vocab: &'a Vocab,
    map: &'a ControlMap,
}

impl<'a> VocabDecoder<'a> {
    pub fn new(vocab: &'a Vocab, map: &'a ControlMap) -> Self {
        VocabDecoder { vocab, map }
    }
}

impl std::fmt::Debug for VocabDecoder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VocabDecoder")
            .field("vocab", &self.vocab.source())
            .field("controls", &self.map.len())
            .finish()
    }
}

impl TokenDecoder for VocabDecoder<'_> {
    /// `render_special = true`: this reproduces the exact bytes that were sent,
    /// which is what a round-trip check compares against.
    ///
    /// The trait's signature is infallible, so a detokenization that fails becomes
    /// a **visible** marker rather than a silent gap. An id the vocabulary rejects
    /// is a wiring bug; text that quietly loses a span is the failure class this
    /// crate exists to abolish, and the two must not look the same downstream.
    fn decode(&self, tokens: &[u32]) -> String {
        match self.vocab.detokenize(tokens, true) {
            Ok(s) => s,
            Err(e) => format!("\u{fffd}<undecodable {} token(s): {e}>", tokens.len()),
        }
    }

    fn control_role(&self, token: u32) -> Option<ControlRole> {
        self.map.role_of(token)
    }
}

#[derive(Debug)]
pub enum SpanTokenizeError {
    /// A `RenderSpan::Control` whose literal is not in the `ControlMap`. Means
    /// the map was built from a different `ControlTokens` set than the dialect
    /// that produced these spans -- a wiring bug, not a data-dependent one.
    UnmappedControl {
        role: ControlRole,
        literal: std::borrow::Cow<'static, str>,
    },
    Vocab(crate::vocab::VocabError),
}

impl std::fmt::Display for SpanTokenizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpanTokenizeError::UnmappedControl { role, literal } => write!(
                f,
                "control token {role:?} {literal:?} is not in this ControlMap; \
                 the map and the dialect disagree"
            ),
            SpanTokenizeError::Vocab(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SpanTokenizeError {}

/// Turn a dialect's rendered spans into token ids.
///
/// This is the whole seam between W4 and W5, and it is three lines because the
/// hard part was done in the type: `Text` and `Control` are different variants,
/// so there is no branch here that could be taken wrongly by adversarial
/// content. The only way to get a control id out of this function is to have
/// put a `RenderSpan::Control` into it.
pub fn tokenize_spans(
    vocab: &Vocab,
    map: &ControlMap,
    spans: &[RenderSpan],
) -> Result<Vec<TokenId>, SpanTokenizeError> {
    let mut out = Vec::new();
    for span in spans {
        match span {
            RenderSpan::Text(text) => {
                out.extend(
                    vocab
                        .tokenize_text(text)
                        .map_err(SpanTokenizeError::Vocab)?,
                );
            }
            RenderSpan::Control(control) => {
                let id = map.id(&control.literal).ok_or_else(|| unmapped(control))?;
                out.push(id);
            }
        }
    }
    Ok(out)
}

fn unmapped(control: &ControlToken) -> SpanTokenizeError {
    SpanTokenizeError::UnmappedControl {
        role: control.role,
        literal: control.literal.clone(),
    }
}
