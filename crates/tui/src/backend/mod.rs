//! **The terminal, and nothing about the conversation.**
//!
//! Everything here talks to the operator's terminal and knows nothing about sessions, rows or
//! the app's state: raw mode and the modes this head turns on, the row-diffing painter
//! ([`terminal`]), what the bytes the terminal sends mean ([`decode`]), which of its extras the
//! terminal speaks ([`features`]), and the two protocols the head draws through beyond cells —
//! kitty graphics ([`graphics`]) and OSC 8 hyperlinks ([`links`]).
//!
//! The dependency runs one way: the app and the ui use this module, and this module uses only
//! the app's [`crate::app::Key`], the vocabulary its decoder produces.

pub mod decode;
pub mod features;
pub mod graphics;
pub mod links;
pub mod terminal;
