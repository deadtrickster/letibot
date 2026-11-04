//! Repair a transcript whose rows were written under a different prefix than the one it names,
//! by forking it onto the prefix that re-chains it instead of refusing the session for ever.
//!
//! The rows are not damaged: each is correctly hashed from some prefix, and the repair copies
//! them rather than re-rendering them, because no renderer is guaranteed to reproduce an
//! assistant row. Nothing stored is rewritten, so a wrong diagnosis cannot destroy the only
//! copy of a conversation, and the fork hangs off the same session row, so the board, the jobs
//! and the title all stand.
//!
//! Only one shape is repaired: row 0 starts past the end of the named prefix, and a stored
//! prefix of exactly that length re-chains every row. Anything else keeps the honest refusal.
//!
//! ```text
//! s-gatekeeper#t0 did not restore: row 0 (s-gatekeeper#t0.0) starts at 32162, should start at 24407
//! ```
//!
//! A broken link mid-chain, a row inside the prefix, a length no prefix has: those are shapes
//! nothing produces, and guessing at a conversation's history is not a repair.

use letibot_tokencore::TokenId;
use letibot_tokencore::ledger::{LedgerError, LedgerRow};
use letibot_tokencore::store::{LoadedTranscript, Store, StoreError};
use letibot_transcript::TranscriptItem;
use letibot_turn::Session;
use letibot_turn::resume::{RestoreError, restore_parts};

/// What a repair produced: the conversation, where it now lives, and what to tell the operator.
pub(crate) struct Repaired {
    pub session: Session,
    /// The new `<session>#tN` the rows were carried onto - now the session's conversation.
    pub transcript_id: String,
    /// The prefix those rows were actually written under, which the fork is seated with.
    pub prefix_id: String,
    /// One sentence for the resume report, naming all three numbers.
    pub note: String,
}

/// Repair `loaded` if, and only if, it is the shape described in the module header. `Ok(None)` is not this shape or
/// not provably repairable, so the caller keeps its refusal; `Err` is a store that could not be read or written.
pub(crate) fn rows_on_another_prefix(
    store: &Store,
    session_id: &str,
    loaded: &LoadedTranscript,
    cause: &RestoreError,
) -> Result<Option<Repaired>, StoreError> {
    let RestoreError::Chain(LedgerError::NotContiguous {
        index: 0,
        expected_offset,
        found_offset,
        ..
    }) = cause
    else {
        return Ok(None);
    };
    let (found, expected) = (*found_offset, *expected_offset);
    // A row that starts at or inside the named prefix is a shape this module does not explain,
    // so both stay refused rather than guessed at.
    if found <= expected {
        return Ok(None);
    }
    let items: Vec<TranscriptItem> = loaded
        .items
        .iter()
        .map(|(item, _, _)| item.clone())
        .collect();
    let rows = loaded.ledger_input();

    // `#tN`, counted the way every other fork counts it, so two forks cannot collide on an id.
    let n: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM transcript WHERE session_id = ?1",
        [session_id],
        |r| r.get(0),
    )?;
    let new_id = format!("{session_id}#t{n}");

    // Rebuilding the chain is the only test that tells two prefixes of one length apart; newest
    // first, because a mis-seated transcript was written under a then-recent build's prompt.
    let mut chosen = None;
    for id in store.stable_prefix_ids_of_len(found)? {
        let rec = store.stable_prefix_record(&id)?;
        if let Ok(session) = restore_parts(&new_id, &rec.tokens, rec.h_init, &rows, items.clone()) {
            chosen = Some((id, rec.tokens.len(), session));
            break;
        }
    }
    let Some((prefix_id, prefix_len, session)) = chosen else {
        return Ok(None);
    };
    let carried_rows = session.items.len();

    // Unsealed until every row is in, then sealed: a daemon that dies mid-carry reopens on the
    // old transcript rather than on part of this one.
    store.put_fork_unsealed(
        &new_id,
        session_id,
        &prefix_id,
        &loaded.transcript_id,
        loaded.items.len() as u32,
    )?;
    let carried: Vec<(&TranscriptItem, &LedgerRow, &[TokenId])> = session
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let row = session.ledger.rows().get(i)?;
            let tokens = session.ledger.item_tokens(i)?;
            Some((item, row, tokens))
        })
        .collect();
    store.append_items(&new_id, 0, carried)?;
    store.seal_transcript(&new_id)?;

    let note = format!(
        "transcript {} could not be rebuilt and was FORKED, not refused: its first row starts at \
         token {found} while the prefix it named is {expected} tokens long, so every one of its \
         {carried_rows} row(s) began past the end of the prompt a resume rebuilds. That is the \
         shape a daemon makes when it opens a session that has no rows yet and appends them \
         under the prompt the BUILD composes rather than the one the transcript was seated with. \
         The rows themselves are intact — they chain from `{prefix_id}` ({prefix_len} tokens), \
         which is in the store — so this conversation continues as {new_id}: the same session, \
         the same items and the same tokens, a fork whose parent is {}. {} is left exactly as it \
         was.",
        loaded.transcript_id, loaded.transcript_id, loaded.transcript_id,
    );
    Ok(Some(Repaired {
        session,
        transcript_id: new_id,
        prefix_id,
        note,
    }))
}
