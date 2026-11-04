//! The GitHub reviewer seat: one open pull request, one letibot session, one review.
//! [`source`] is the trait, [`fake`] the test source, [`gh`] a subprocess whose command line tests assert.
//! [`seat`] is the poll. Until an identity named `letibot` exists every review is a draft.

pub mod fake;
pub mod gh;
pub mod seat;
pub mod source;
pub mod worker;

pub use seat::{Config, Draft, DraftView, PullRow, ReviewState, Seat, Session, Snapshot};
pub use source::{Comment, Event, Inline, Pull, Review, Scope, Source, Standing};
pub use worker::{Sessions, Worker};
