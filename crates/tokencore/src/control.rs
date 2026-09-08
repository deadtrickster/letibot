//! Resolving a dialect's control tokens once, at startup, and tokenizing a
//! rendered span sequence.
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

use std::collections::HashMap;

use letibot_dialect::{ControlRole, ControlToken, ControlTokens, RenderSpan};

use crate::vocab::{ResolveCause, TokenId, Vocab};

/// Every control token of one dialect, resolved to exact ids.
///
/// Constructed only by [`resolve`], so a `ControlMap` in hand is proof that the
/// vocabulary agreed with the dialect at startup.
#[derive(Debug, Clone)]
pub struct ControlMap {
    by_literal: HashMap<&'static str, TokenId>,
    by_role: HashMap<ControlRole, TokenId>,
}

impl ControlMap {
    pub fn id(&self, token: ControlToken) -> Option<TokenId> {
        self.by_literal.get(token.literal).copied()
    }

    pub fn role(&self, role: ControlRole) -> Option<TokenId> {
        self.by_role.get(&role).copied()
    }

    pub fn len(&self) -> usize {
        self.by_role.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_role.is_empty()
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
        self.by_literal.values().any(|&t| t == id)
    }
}

/// One literal that would not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFailure {
    pub role: ControlRole,
    pub literal: &'static str,
    pub cause: ResolveCause,
}

/// Every failure at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlResolveError {
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

/// Resolve a dialect's whole control-token set against a vocabulary.
///
/// Call this once, at startup, before any ledger exists.
pub fn resolve(vocab: &Vocab, tokens: ControlTokens) -> Result<ControlMap, ControlResolveError> {
    let mut by_literal = HashMap::new();
    let mut by_role = HashMap::new();
    let mut failures = Vec::new();

    for token in tokens.0.iter().copied() {
        match vocab.resolve_control(token.literal) {
            Ok(id) => {
                by_literal.insert(token.literal, id);
                by_role.insert(token.role, id);
            }
            Err(cause) => failures.push(ControlFailure {
                role: token.role,
                literal: token.literal,
                cause,
            }),
        }
    }

    if failures.is_empty() {
        Ok(ControlMap { by_literal, by_role })
    } else {
        Err(ControlResolveError { failures })
    }
}

/// Resolve a dialect's stop-token literals the same way.
///
/// Same rules, same startup timing. A stop token that is silently a sequence
/// never fires, and a turn that never stops is a turn that runs to `n_ctx`.
pub fn resolve_stops(
    vocab: &Vocab,
    literals: &'static [&'static str],
) -> Result<Vec<TokenId>, ControlResolveError> {
    let mut ids = Vec::with_capacity(literals.len());
    let mut failures = Vec::new();
    for literal in literals.iter().copied() {
        match vocab.resolve_control(literal) {
            Ok(id) => ids.push(id),
            Err(cause) => failures.push(ControlFailure {
                // Stop tokens have no role in the dialect vocabulary; TurnEnd is
                // the closest honest label and it is only used for the message.
                role: ControlRole::TurnEnd,
                literal,
                cause,
            }),
        }
    }
    if failures.is_empty() {
        Ok(ids)
    } else {
        Err(ControlResolveError { failures })
    }
}

#[derive(Debug)]
pub enum SpanTokenizeError {
    /// A `RenderSpan::Control` whose literal is not in the `ControlMap`. Means
    /// the map was built from a different `ControlTokens` set than the dialect
    /// that produced these spans -- a wiring bug, not a data-dependent one.
    UnmappedControl { role: ControlRole, literal: &'static str },
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
                let id = map.id(*control).ok_or(SpanTokenizeError::UnmappedControl {
                    role: control.role,
                    literal: control.literal,
                })?;
                out.push(id);
            }
        }
    }
    Ok(out)
}
