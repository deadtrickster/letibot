//! **The image channel, re-exported from the crate that owns the vocabulary.**
//!
//! `Media` lives in `letibot_transcript::media` because the *log* stores it and the *tool layer*
//! produces it, and this layer depends on that one rather than the other way round — the same call
//! this tree made for `ToolEditExcerpt`, whose docstring says it: *"the one crate this crate and the
//! runtime can both see."*
//!
//! Read `letibot_transcript::media` for the design: the operator's ruling that a read is a read, the
//! measurement that a `data:` URI is the form the server takes and a bare path is refused, and the
//! measured size behaviour that says why there is no cap here.
//!
//! Kept as a module rather than replaced by a direct import at each call site so that the tool
//! layer keeps one place to look for *what does `read` return when the answer is a picture*.
pub use letibot_transcript::media::{Media, SUPPORTED_IMAGE_MIMES, encode_base64, sniff_mime};
