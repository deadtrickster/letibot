//! The GitHub reviewer seat: one open pull request, one letibot session, one review.
//!
//! **Nothing here talks to the real GitHub.** [`source`] is the trait, [`fake`] is the source every
//! test uses, and [`gh`] is `gh api` as a subprocess whose command line a test asserts and never
//! runs. [`seat`] is the poll: what is open, what changed, what is owed a review, and what is owed
//! an answer - and, until an identity named `letibot` exists, every review is kept as a draft.

pub mod fake;
pub mod gh;
pub mod seat;
pub mod source;
pub mod worker;

pub use seat::{Config, Draft, PullRow, ReviewState, Seat, Session, Snapshot};
pub use source::{Comment, Event, Inline, Pull, Review, Scope, Source, Standing};
pub use worker::{Sessions, Worker};
