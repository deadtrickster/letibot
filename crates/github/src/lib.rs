//! The GitHub reviewer seat: one open pull request, one letibot session, one review.
//!
//! **Nothing here talks to the real GitHub.** [`source`] is the trait, [`fake`] is the source every
//! test uses, and [`gh`] is `gh api` as a subprocess whose command line a test asserts and never
//! runs.

pub mod fake;
pub mod gh;
pub mod source;

pub use source::{Comment, Event, Inline, Pull, Review, Scope, Source, Standing};
