//! The image channel, re-exported from the crate that owns the vocabulary, so the tool
//! layer keeps one place to look for what `read` returns when the answer is a picture.
//! The design lives in `letibot_transcript::media`; this module adds nothing to it.
pub use letibot_transcript::media::{Media, SUPPORTED_IMAGE_MIMES, encode_base64, sniff_mime};
