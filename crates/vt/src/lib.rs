//! A terminal screen, modelled: the bytes a full-screen program writes, applied to a grid a head
//! can read.
//!
//! # Why this exists
//!
//! The operator's ask, in their words: *"if i run `mc` the conversation window replaced by mc but
//! prompt area stays"*. A pane that shows a full-screen program needs two pieces that did not
//! exist in this tree: **a session the program keeps running in**, and **a screen to draw it
//! into**. This crate is the second, and it is deliberately the half that needs nothing else —
//! `Screen::feed` takes bytes, `Screen::rows` gives cells back, and no file descriptor, thread,
//! clock or session appears anywhere in it. The first piece is its own crate; nothing here knows
//! what a pty is, and that is what makes every test in this crate a `feed` and an assertion.
//!
//! # What a head does with it
//!
//! ```text
//!     read bytes from the program ──▶ Screen::feed ──▶ rows()/line() ──▶ a frame
//! ```
//!
//! and the two facts a head needs from the model rather than from the program:
//!
//! - [`Screen::alternate`] — the alternate screen is up, so the pane is drawing a *program* and
//!   the transcript is parked behind it, intact. That is the operator's sentence exactly: the
//!   conversation window is replaced, the prompt area stays.
//! - [`Screen::cursor`] and [`Screen::cursor_visible`] — where the program left the cursor and
//!   whether it wants one. **Whether the pane should show it is the head's decision and not this
//!   crate's**: a pane shows a cursor when it has the focus, and focus is a fact this crate does
//!   not have.
//!
//! # The one vocabulary, and where the head's half lives
//!
//! A cell carries the terminal's own vocabulary — a foreground slot `0`–`15`, a background slot
//! `0`–`15`, bold, dim, reverse —
//! and nothing here names a *meaning*. The head's vocabulary is `letibot_ui::style::Role`
//! (*"something failed"*, *"this is syntax"*), and the mapping between them has one definition:
//! [`crate::attr`] owns the walk that reads an `SGR` parameter list into a pen, and
//! `letibot_ui::ansi` owns which role a pen is drawn as. That is why `letibot-ui` depends on this
//! crate rather than the other way round, and why the walk is here rather than duplicated: a
//! payload line and a full screen are two readers of the same parameters.
//!
//! # What this crate will not show, and why the list is in the code
//!
//! Mouse reporting, bracketed paste, hyperlinks, truecolour and 256-colour fidelity, fonts,
//! underline and italic, and an answer to a program that asks the terminal a question. Each is
//! stated where it is dropped — [`crate::attr`] for the pen, [`crate::screen`] for the modes,
//! [`crate::parser`] for the sequences — together with what a pane therefore will not show. The
//! tree's rule is that a deliberate gap is written down where it is made; the summary is
//! [`crate::screen`]'s header.
//!
//! # Map
//!
//! | module | what it owns |
//! |---|---|
//! | [`attr`] | the pen: the sixteen slots foreground and background, four attributes, and the one `SGR` walk |
//! | [`width`] | how many cells a character takes |
//! | [`parser`] | the byte state machine: partial sequences, partial characters, strings |
//! | [`screen`] | the grid, the cursor, the modes, and the sequences that act on them |

pub mod attr;
pub mod parser;
pub mod screen;
pub mod width;

pub use attr::{Attr, Hue, apply_sgr};
pub use parser::{Csi, Event, MAX_PARAMS, Parser};
pub use screen::{Cell, Screen};
pub use width::char_width;
