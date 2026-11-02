//! Rebuilding a [`Session`] from what the store persisted.
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
//! The tokens come from `transcript_item.tokens`, never from a renderer: an
//! assistant row's ids were cut from what the server streamed, and no renderer
//! reproduces them. There is no `PromptRenderer` parameter here; that absence is
//! the guarantee.
//!
//! [`TokenLedger::restore`] re-chains the persisted rows and
//! [`TokenLedger::verify_chain`] recomputes from the region the session will
//! submit. A restore whose rows and items disagree is refused, because downstream
//! indexes both with one number. `turn_seq` starts at the restored item count: a
//! watermark, so ids never repeat or go backwards across a restart.

use letibot_tokencore::TokenId;
use letibot_tokencore::ledger::{LedgerError, LedgerRow, TokenLedger};
use letibot_tokencore::store::LoadedTranscript;
use letibot_transcript::TranscriptItem;

use crate::engine::Session;

/// Why a stored session could not be rebuilt.
///
/// Every variant names the row: the first question after "it refused" is which turn.
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
    /// The one entry point the acceptance test and the daemon both use.
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
/// A caller holding the rows from elsewhere - a fork, a test - need not fake a store row.
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
