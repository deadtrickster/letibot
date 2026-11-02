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
//! mode = "http://127.0.0.1:8080"   # an OpenAI-compatible /v1/embeddings
//! ```
//!
//! Nothing is compiled in and nothing is downloaded at build time: the built-in model
//! is fetched once into the data directory, and a box with no model and no URL says so
//! by name instead of scoring nothing.
//!
//! The cache is a cache: `<data dir>/embeddings.db`, keyed by `(model, sha256 of the
//! text)`, so an edited note is a new key and a deleted file costs time and no more.

pub mod config;

pub use config::{ConfigError, Mode, Settings};
