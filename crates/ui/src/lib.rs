//! Rendering primitives for a letibot head: this crate produces lines, and a head decides
//! where they go. Nothing here opens a file descriptor or knows what a session is, which is
//! why all of it is testable without a terminal.
//!
//! Modules carrying an idea, an algorithm or a constant from grok-build (xAI, Apache-2.0) or
//! opencode (MIT, read through the Kilo Code fork) say so in their own header, name the
//! upstream file and state what was changed - Apache-2.0 section 4(b) requires the last of
//! those. Modules with no such header are original to letibot; the credits are in `NOTICE`.

pub mod ansi;
pub mod editor;
pub mod painter;
/// **Text this head did not author, made safe for a terminal** (§3.1). Here rather than
/// in a head, because this crate draws every card and had no sanitiser at all.
pub mod text;
