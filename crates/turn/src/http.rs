//! The blocking HTTP/1.1 client, re-exported from `letibot-http`.
//!
//! It used to live here. It moved when the flowy connector needed the same
//! client with an `Authorization` header and a `DELETE` verb, and the note that
//! stood at the bottom of the old file — *"a second hand-rolled HTTP client is a
//! second place for a framing bug to live"* — is the reason it moved rather than
//! being copied. Every path in this crate still spells `crate::http::…`.

pub use letibot_http::*;
