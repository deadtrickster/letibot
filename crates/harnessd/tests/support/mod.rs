//! Fixtures that more than one integration binary spelled out identically, included with
//! `mod support;`.
//!
//! A fixture belongs here when three or more binaries carried the same bytes: the canned Qwen
//! turn a replayed provider answers with, the HTTP stub that serves it, and the vocabulary path
//! a harness opens with. A helper whose copy differs on purpose stays in its own file, where the
//! difference can be read beside the assertion that depends on it.

// A fixture, not a library: each binary includes this file and uses the part its subject needs,
// so the rest is dead in that compilation and alive in another.
#![allow(dead_code)]

/// The turn crate's one-shot SSE server, shared by path so a second copy of the wire shape
/// cannot drift from the first.
#[path = "../../../turn/tests/support/canned.rs"]
pub mod canned;

pub mod qwen;
pub mod room;
pub mod stub;
pub mod vocab;
