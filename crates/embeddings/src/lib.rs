//! The second similarity tier: a text as a vector, and two vectors as a cosine.
//!
//! `letibot_tools::similarity` scores two texts on the terms they share, and its own
//! measurement says where that stops: a paraphrase of a note scored 0.126 under a
//! docker prompt's 0.133 against an unrelated note, so no floor separates them. This
//! crate is the tier that answers for meaning rather than for words, and it answers
//! in three modes read from one key:
//!
//! ```toml
//! [embeddings]
//! mode = "off"          # lexical only, and the default
//! mode = "internal"     # candle, in this process, with the built-in model
//! model = "<dir>"       # ... or a directory with config.json, tokenizer.json, model.safetensors
//! download = false      # ... and never fetch the built-in model on a box without a network
//! mode = "http://127.0.0.1:8080"   # an OpenAI-compatible /v1/embeddings
//! ```
//!
//! Nothing is compiled in: the built-in model is fetched once into the data directory, from a
//! pinned revision and checked against a pinned hash per file, and a box that wrote
//! `download = false` is never fetched for at all.
//!
//! The cache is a cache: `<data dir>/embeddings.db`, keyed by `(model, sha256 of the
//! text)`, so an edited note is a new key and a deleted file costs time and no more.

pub mod builtin;
pub mod cache;
pub mod config;
pub mod cosine;
pub mod model;
pub mod remote;
pub mod tier;

/// A tiny BERT a test builds: the internal path without a download, for this crate's
/// tests and for a crate that scores with it.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// A scratch directory, the fixture checkpoint and a loopback server: how a test measures a
/// fetch without the network.
#[cfg(test)]
mod fake;

pub use builtin::{DownloadError, Pin, Source};
pub use cache::{Cache, CacheError, Hit, db_path};
pub use config::{ConfigError, Mode, Settings};
pub use cosine::{Index, cosine};
pub use model::{BUILTIN_MODEL, Model, ModelError, builtin_dir};
pub use remote::{Remote, RemoteError};
pub use tier::{Embedder, Engine, Filled, Planted, Tier, TierError};
