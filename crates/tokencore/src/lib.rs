//! The token core: vocabulary, tokenizer, token ledger and store.
//!
//! This is the crate where a conversation stops being text and becomes the thing the server
//! accounts for: ids, their region, the chain over them, and the SQLite store that keeps them.
//! Everything here is built so a restart can rebuild the region by replaying the ledger's
//! spans rather than re-rendering, which is why the tokens are stored at all.

pub mod apparatus;
pub mod control;
pub mod ledger;
pub mod region;
pub mod store;
pub mod vocab;

pub use control::{
    ControlMap, ControlResolveError, VocabDecoder, resolve, resolve_stops, tokenize_spans,
};
pub use ledger::{LedgerRow, PromptSpan, TokenLedger, chain, hash_tokens};
pub use region::TokenRegion;
/// The SQL driver, re-exported: `Store::connection` hands out a `&rusqlite::Connection`, so the
/// type is public API here whether or not the name is.
pub use rusqlite;
pub use store::{
    MergePriority, MergeRow, MergeState, ReviewRow, ReviewState, SessionRecord, StablePrefixRecord,
    Store,
};
pub use vocab::{
    ATTR_CONTROL, ATTR_USER_DEFINED, BYTES_SOURCE, ResolveCause, TokenId, Vocab, VocabBackend,
    VocabError,
};
