//! The GitHub reviewer seat: one open pull request, one letibot session, one review.
//! [`source`] is the trait, [`fake`] the test source, [`gh`] a subprocess whose command line tests assert.
//! [`seat`] is the poll. Until an identity named `letibot` exists every review is a draft.

pub mod fake;
pub mod gh;
pub mod mirror;
pub mod offers;
pub mod seat;
pub mod source;
pub mod worker;

pub use mirror::{Mirrors, github_url, slug_of_url};
pub use offers::{Candidate, Candidates, Kind, Offer, Scorer};
pub use seat::{Config, Draft, DraftView, PullRow, ReviewState, Seat, Session, Snapshot};
pub use source::{Comment, Event, Inline, Pull, Review, Scope, Source, Standing};
pub use worker::{Sessions, Worker, inline_in};
