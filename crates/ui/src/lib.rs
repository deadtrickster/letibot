//! Rendering primitives for a letibot head: this crate produces lines, a head decides where they go.
//! Modules derived from grok-build (xAI, Apache-2.0) or opencode (MIT, via Kilo Code) say so in
//! their header (Apache-2.0 4(b)); others are original. Credits are in `NOTICE`.

pub mod ansi;
pub mod editor;
pub mod painter;
/// Text this head did not author, made safe for a terminal. Here rather than
/// in a head, because this crate draws every card and had no sanitiser at all.
pub mod text;
