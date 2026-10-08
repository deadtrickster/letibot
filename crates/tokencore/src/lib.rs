//! The token core: vocabulary, tokenizer, token ledger and store.
//!
//! This is the crate where a conversation stops being text and becomes the
//! thing the server actually caches. Under §3.1 the harness renders and
//! tokenizes for itself, so the artefact that has to be append-only is not a
//! byte string it *hopes* a remote jinja renderer preserves -- it is one growing
//! `Vec<llama_token>` that both sides address by offset and length.
//!
//! Four pieces, in the order a turn goes through them:
//!
//! * [`vocab::Vocab`] -- the vocabulary, answered by a backend: a `vocab_only` GGUF
//!   load through llama.cpp (`letibot-llama`, measured at 0.6 s against shard 1 of a
//!   six-way split, no weights, no GPU, no server), or the byte vocabulary
//!   ([`Vocab::bytes`]) for a session on a cloud provider. **This crate links no
//!   llama.cpp**: only `letibot-llama` does, so the store, the ledger and everything
//!   built on them carry no C++ runtime and no shared library.
//! * [`control`] -- resolves a dialect's control literals to exact ids **at
//!   startup**, and turns `RenderSpan`s into tokens across the one seam where
//!   text and control tokens must not be able to become each other.
//! * [`ledger::TokenLedger`] -- the rows, the hash chain, and the memfd region
//!   whose shape makes a prefix violation blocked rather than merely
//!   detectable.
//! * [`store::Store`] -- the durable copy, with the append-only rule restated as
//!   SQL triggers.
//!
//! # The one thing to keep in mind while changing this crate
//!
//! Every safety property here is structural, and each one is written where it
//! cannot be argued with:
//!
//! | the property | where it lives |
//! |---|---|
//! | user text cannot become a control token | two functions, one with `parse_special` permanently off ([`vocab`]) |
//! | a missing control token is a startup failure | [`control::resolve`], which reports *all* of them |
//! | the token file cannot shrink | `F_SEAL_SHRINK`, in the kernel ([`region`]) |
//! | a reader cannot write to it | an `O_RDONLY` reopen; `PROT_WRITE` gets `EACCES` ([`region`]) |
//! | a request cannot start anywhere but 0 | [`ledger::PromptSpan`] has no offset field |
//! | history cannot be rewritten in this process | one mutation, pinned by a test that reads its own source |
//! | history cannot be rewritten in the database | `BEFORE UPDATE/DELETE/INSERT` triggers ([`store`]) |
//! | a rewritten store cannot be replayed | the chain is re-verified on restore |
//!
//! If a change makes one of those a matter of calling the right function, it has
//! undone the point of the strand rather than refactored it.

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
/// **The SQL driver, re-exported.** `Store::connection` already hands out a
/// `&rusqlite::Connection`, so the type is public API here whether or not the
/// name is. A caller that needs to write a query against it — the daemon's
/// corpus reader does — would otherwise take its own `rusqlite` dependency, and
/// two versions of it in one tree makes `&Connection` and `&Connection` two
/// unrelated types with one spelling.
pub use rusqlite;
pub use store::{MergeEntry, MergePriority, MergeState, SessionRecord, StablePrefixRecord, Store};
pub use vocab::{
    ATTR_CONTROL, ATTR_USER_DEFINED, BYTES_SOURCE, ResolveCause, TokenId, Vocab, VocabBackend,
    VocabError,
};
