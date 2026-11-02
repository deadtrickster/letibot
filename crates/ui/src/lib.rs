//! Rendering primitives for a letibot head.
//!
//! `letibot-tui` is one head: a socket, a `termios`, an event loop and a state machine that turns
//! frames into a screen. The things here are none of those. Measuring a string in terminal columns,
//! colouring a code block, laying out a diff and editing a line are library problems with no opinion
//! about where the bytes came from, and burying them in one head's loop means the second head, a
//! pager or a log viewer or a replay renderer, either forks them or does without.
//!
//! This crate produces lines; a head decides where they go. Nothing here opens a file descriptor,
//! reads the environment, or knows what a session is, which is also why all of it is testable without
//! a terminal.
//!
//! Two upstream projects were read for this crate and are credited in the top-level `NOTICE`:
//! grok-build (xAI, Apache-2.0) and opencode (MIT), read through the Kilo Code fork which retains
//! opencode's copyright line. A module carrying an idea, an algorithm or a constant from either names
//! the upstream file in its own header and states what was changed, which Apache-2.0 section 4(b)
//! requires. A module with no such header is original to letibot.
//!
//! | module | what it owns |
//! |---|---|
//! | [`editor`] | multi-line input, history, paste, kill ring |
//! | [`painter`] | string painting that closes a span back to its block (the role table is `rano::style`) |
//! | [`ansi`] | SGR a foreign program wrote, drawn as the palette's own roles |
//!
//! The tool card, the diffs, the capture to role table and the prefill bar live in `rano`, ported
//! from this crate with their tests: every terminal rendering has one home, and a head maps its state
//! onto rano's view models rather than carrying its own painter.
//!
//! A program's whole screen is `letibot-vt`, a crate below this one, so the head depends on the model
//! rather than the model on the head. This crate owns the head's half: [`ansi::lines`] turns a
//! screen's cells into rows.

pub mod ansi;
pub mod editor;
pub mod painter;
/// Text this head did not author, made safe for a terminal. Here rather than in a head, because
/// this crate draws every card and had no sanitiser at all.
pub mod text;
