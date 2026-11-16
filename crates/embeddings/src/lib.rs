//! The second similarity tier: a text as a vector, and two vectors as a cosine, for meaning where
//! the lexical tier only matches words. Configured by `[embeddings]` ([`config`]). Nothing is compiled
//! in, and the cache is `<data dir>/embeddings.db` keyed by `(model, sha256 of the text)`.

pub mod builtin;
pub mod cache;
pub mod config;
pub mod cosine;
pub mod model;
pub mod remote;
pub mod tier;
pub mod wordfreq;

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
pub use config::{ConfigError, Mode, PinOverride, Settings};
pub use cosine::{Index, cosine};
pub use model::{BUILTIN_MODEL, Model, ModelError, builtin_dir};
pub use remote::{Remote, RemoteError};
pub use tier::{Embedder, Engine, Filled, Planted, Spent, Tier, TierError};
