//! **A transcript whose rows do not rebuild under the prefix it names — repaired by a fork,
//! not refused for ever.**
//!
//! # The shape this exists for, measured
//!
//! MEASURED 2026-10-26 in the operator's own store: `s-gatekeeper#t0` was seated 2026-10-11
//! under a 24,407-token stable prefix and had **no rows**. A daemon running newer binaries
//! opened it on 2026-10-25, composed its own 32,162-token prompt, and appended fourteen rows
//! starting at 32162. Nothing refused them: the store's append-only trigger checks that a row
//! continues the one before it and that `seq` is the next, and row 0 has nothing before it, so
//! row 0 could start anywhere. The transcript has been unreadable by a resume ever since:
//!
//! ```text
//! s-gatekeeper#t0 did not restore: row 0 (s-gatekeeper#t0.0) starts at 32162, should start at 24407
//! ```
//!
//! `Store::assert_anchored` is what stops the writing now, and `Harness::open` re-seats an
//! empty transcript so this is never created again. That leaves the stores already in this
//! state — and a refusal there is a conversation the operator can never open again, which is a
//! worse answer than the repair this module makes.
//!
//! # What the repair is, and what it is not
//!
//! The rows are not damaged. Each one is correctly hashed from *some* prefix, and this module
//! finds that prefix — a stored `stable_prefix` of exactly the length row 0 starts at whose
//! `h_init` re-chains every row — and then **forks the transcript onto it**: a new
//! `<session>#tN` carrying the same items and the same tokens, exactly as a re-seat carries a
//! conversation.
//!
//! * **Nothing is re-rendered.** The stored tokens are what the model was actually shown, and
//!   the chain proves they belong to the prefix we found, so they are copied rather than
//!   rendered (`resume.rs`'s rule: no renderer is guaranteed to reproduce an assistant row).
//! * **Nothing stored is rewritten.** The old transcript keeps its prefix, its rows and its
//!   chain — it is append-only — so the repair is auditable and a wrong diagnosis cannot
//!   destroy the only copy of a conversation. A transcript that cannot be repaired keeps the
//!   honest refusal it had.
//! * **It is the same session.** The new transcript hangs off the same session row with the old
//!   one as its parent, so the board, the jobs, the children and the title all stand.
//!
//! # Only the exact shape is repaired
//!
//! A row 0 that starts **past** the end of the prefix the transcript names, with a stored
//! prefix of exactly that length that re-chains it. Anything else — a broken link mid-chain, a
//! row that starts *inside* the prefix, a length no stored prefix has — keeps the older,
//! honest refusal: those are shapes nothing in this tree is known to produce, and guessing at
//! a conversation's history is not a repair.

use letibot_tokencore::TokenId;
use letibot_tokencore::ledger::{LedgerError, LedgerRow};
use letibot_tokencore::store::{LoadedTranscript, Store, StoreError};
use letibot_transcript::TranscriptItem;
use letibot_turn::Session;
use letibot_turn::resume::{RestoreError, restore_parts};

/// What a repair produced: the conversation, where it now lives, and what to tell the operator.
pub(crate) struct Repaired {
    pub session: Session,
    /// The new `<session>#tN` the rows were carried onto — now the session's conversation.
    pub transcript_id: String,
    /// The prefix those rows were actually written under, which the fork is seated with.
    pub prefix_id: String,
    /// One sentence for the resume report, naming all three numbers.
    pub note: String,
}

/// **Repair `loaded` if, and only if, it is the shape described in the module header.**
///
/// `Ok(None)` means "not this shape, or not provably repairable" and the caller keeps its
/// refusal. `Err` is a store that could not be read or written, which is not a diagnosis.
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
    // **The prefix the transcript names must be the SHORTER one.** Equal is the trigger's
    // ordinary refusal, and a row starting inside the prefix is a shape with no explanation
    // here; both stay refused.
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

    // **The only test that can tell two prefixes of one length apart**: rebuild the chain.
    // Newest first (the store's order), because the prompt a mis-seated transcript was written
    // under is the one a build was composing then.
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

    // **Unsealed until every row is in (R91.2)**, then sealed: a daemon that dies during the
    // carry reopens on the old transcript rather than on part of this one.
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
