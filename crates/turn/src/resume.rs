//! Rebuilding a [`Session`] from what the store persisted (§4.3 clause 6).
//!
//! ```text
//!   store ──load_transcript──> prefix tokens, h_init, [(row, tokens)], [items]
//!                                              │
//!                    TokenLedger::restore ─────┤  replays the spans, re-hashes each link
//!                                              │
//!                    verify_chain ─────────────┤  recomputes from the rebuilt region
//!                                              │
//!                                          Session
//! ```
//!
//! # The thing this module exists not to do
//!
//! `harnessd`'s startup banner used to carry a disclosure that was correct and
//! expensive:
//!
//! > *No resume from the store. The transcript, the ledger rows and the token blobs
//! > are all persisted and `Store::load_transcript` + `TokenLedger::restore` would
//! > rebuild them — what is missing is a constructor for `letibot_turn::Session` from
//! > a restored ledger, and the alternative (replaying the items through
//! > `append_items`) **re-renders the assistant rows**, which the engine deliberately
//! > never does.*
//!
//! That reasoning holds and this module does not work around it. An assistant row's
//! tokens were **cut from the ids the server streamed** (`crate::items`), not
//! produced by a renderer, and no renderer is guaranteed to reproduce them: whether
//! a stop token was stripped, which `<think>` opened which segment, how a tool
//! call's argument JSON spelled its key order — every one of those is a byte
//! difference the chain would catch on the next turn and nobody would catch during
//! recovery.
//!
//! So the tokens come from `transcript_item.tokens`, which is the column the store's
//! own header says exists for exactly this ("§4.3 clause 6 says a restart rebuilds
//! the region *by replaying the ledger's spans*, not by re-rendering"). Nothing here
//! calls a `PromptRenderer`, and there is no `TurnEngine` parameter — the absence of
//! one is the guarantee, in the same way that `TokenLedger` having no `truncate` is.
//!
//! # Two passes, not one, and they check different things
//!
//! [`TokenLedger::restore`] walks the persisted rows, re-chains each one and refuses
//! on the first mismatch. Then [`TokenLedger::verify_chain`] recomputes the whole
//! chain **from the tokens in the rebuilt region**. The second is not the first
//! repeated: the first checks the rows against the blobs it was handed, the second
//! checks the memfd the session will actually submit. A restore that wrote the right
//! rows over the wrong bytes — a partial `append`, a region that grew wrong — is
//! invisible to the first and fatal to the second, and this is the one moment it is
//! cheap to look.
//!
//! # Why an unpaired restore is refused rather than zipped
//!
//! Two invariants downstream index the ledger and the item list with the *same*
//! number: `Harness::persist` writes `items[i]` beside `rows[i]`, and
//! `Session::append_items` mints the next item id as `"{transcript}.{items.len()}"`.
//! A session restored with more rows than items satisfies neither, and both failures
//! are quiet — the first stores the wrong item next to a row, the second re-uses an
//! item id that is already in the store's unique index. So the pairing is a
//! precondition with an error of its own, not something to fix up.
//!
//! # `turn_seq` is a watermark, not a turn count
//!
//! A turn id is `"{transcript_id}#{turn_seq}"` and turn ids are not persisted —
//! there is no `turn` table yet (`crate::store`'s header lists it among the tables a
//! later strand writes). Resuming with `turn_seq = 0` would mint `#1` a second time
//! for a different turn, and every log, metric and tool-call id keyed on it would
//! then describe two things.
//!
//! So a restored session starts its counter at the number of restored **items**.
//! That is not how many turns happened — it is strictly more, which is the property
//! that matters: within one transcript the ids never repeat, and they never go
//! backwards across a restart. It is deliberately not called a turn count anywhere,
//! because `#57` on a screen must not be read as "the fifty-seventh turn".

use letibot_tokencore::TokenId;
use letibot_tokencore::ledger::{LedgerError, LedgerRow, TokenLedger};
use letibot_tokencore::store::LoadedTranscript;
use letibot_transcript::TranscriptItem;

use crate::engine::Session;

/// Why a stored session could not be rebuilt.
///
/// Every variant names the row. The operator's next question after "it refused" is
/// always "which turn", and an error that cannot answer it sends them to the
/// database with a hex string.
#[derive(Debug)]
pub enum RestoreError {
    /// The chain did not reproduce, or the rows are not contiguous, or a blob and
    /// its row disagree about length. [`LedgerError`] carries the index, the item id
    /// and both hashes.
    Chain(LedgerError),
    /// The ledger rows and the transcript items are not the same length. See the
    /// module header for the two things downstream that index both with one number.
    Unpaired { rows: usize, items: usize },
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::Chain(e) => write!(
                f,
                "this session was NOT resumed: {e}. The store's rows do not rebuild the \
                 conversation they describe, so continuing would submit a prompt whose \
                 history is not the one the model was shown. Nothing was changed."
            ),
            RestoreError::Unpaired { rows, items } => write!(
                f,
                "this session was NOT resumed: {rows} ledger row(s) but {items} transcript \
                 item(s). The two are appended together and are indexed by the same number, \
                 so a session that cannot pair them would store the wrong item beside a row \
                 and re-use an item id. Nothing was changed."
            ),
        }
    }
}

impl std::error::Error for RestoreError {}

impl From<LedgerError> for RestoreError {
    fn from(e: LedgerError) -> Self {
        RestoreError::Chain(e)
    }
}

impl Session {
    /// Rebuild a session from a loaded transcript, tokens and all.
    ///
    /// The whole of resume in one call, so that the acceptance test and the daemon
    /// exercise the same path: a second entry point that "also restores" is a second
    /// one to keep in step, and the one nobody runs is the one that rots.
    ///
    /// Refuses rather than approximates. See [`RestoreError`].
    pub fn restore(loaded: &LoadedTranscript) -> Result<Session, RestoreError> {
        restore_parts(
            &loaded.transcript_id,
            &loaded.prefix_tokens,
            loaded.h_init,
            &loaded.ledger_input(),
            loaded.items.iter().map(|(i, _, _)| i.clone()).collect(),
        )
    }
}

/// [`Session::restore`] with the pieces passed separately.
///
/// Public because a caller that has the rows from somewhere other than
/// [`LoadedTranscript`] — a fork, a test that tampers with one — should not have to
/// build a fake store row to use the same code.
pub fn restore_parts(
    transcript_id: &str,
    prefix_tokens: &[TokenId],
    h_init: [u8; 32],
    rows: &[(LedgerRow, Vec<TokenId>)],
    items: Vec<TranscriptItem>,
) -> Result<Session, RestoreError> {
    if rows.len() != items.len() {
        return Err(RestoreError::Unpaired {
            rows: rows.len(),
            items: items.len(),
        });
    }
    // Pass one: replay the spans into a fresh region, re-chaining every link.
    let ledger = TokenLedger::restore(transcript_id, prefix_tokens, h_init, rows)?;
    // Pass two: recompute from the region the session will submit. See the header
    // for why this is not the first check repeated.
    ledger.verify_chain()?;
    Ok(Session::from_restored(
        transcript_id.to_string(),
        ledger,
        items,
    ))
}
