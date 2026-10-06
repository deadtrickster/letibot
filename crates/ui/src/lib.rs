//! Rendering primitives for a letibot head.
//!
//! # Why this is a crate and not more of `letibot-tui`
//!
//! `letibot-tui` is one head: a socket, a `termios`, an event loop and a state
//! machine that turns frames into a screen. The things in *this* crate are none
//! of those. Measuring a string in terminal columns, colouring a code block,
//! laying out a diff and editing a line are library problems with no opinion
//! about where the bytes came from, and burying them inside one head's loop
//! means the second head — a pager, a log viewer, a `--replay` renderer, the
//! flowy bridge — either forks them or does without.
//!
//! So the split is: **this crate produces lines; a head decides where they go.**
//! Nothing here opens a file descriptor, reads the environment, or knows what a
//! session is. Every entry point is a pure function or a small struct with an
//! explicit `Vec<String>` output, which is also why all of it is testable
//! without a terminal.
//!
//! # Provenance
//!
//! Two upstream projects were read for this crate and are credited in the
//! top-level `NOTICE`:
//!
//! - **grok-build** (xAI, Apache-2.0) — Rust.
//! - **opencode** (MIT), read through the Kilo Code fork, which retains
//!   opencode's copyright line.
//!
//! Modules carrying an idea, an algorithm or a constant from either say so in
//! their own header, name the upstream file, and state what was changed —
//! Apache-2.0 §4(b) requires the last of those and it is the part everyone
//! forgets. Modules with no such header are original to letibot.
//!
//! # Map
//!
//! | module | what it owns |
//! |---|---|
//! | [`width`] | columns, grapheme clusters, escape-aware wrap and truncate |
//! | [`highlight`] | rano capture names → this crate's syntax roles |
//! | [`diff`] | line diff, intra-line word diff, unified rendering |
//! | [`sidediff`] | the two-panel before/after view of a file edit |
//! | [`progress`] | the prefill bar, which needs data neither upstream has |
//! | [`card`] | tool calls: collapsed, expanded, and what a long result looks like |
//! | [`editor`] | multi-line input, history, paste, kill ring |
//! | [`style`] | the one place a colour is chosen |
//! | [`ansi`] | SGR a foreign program wrote, drawn as the palette's own roles |

pub mod ansi;
pub mod card;
pub mod diff;
pub mod editor;
pub mod highlight;
pub mod progress;
pub mod sidediff;
pub mod style;
/// **Text this head did not author, made safe for a terminal** (§3.1). Here rather than
/// in a head, because this crate draws every card and had no sanitiser at all.
pub mod text;
pub mod width;
