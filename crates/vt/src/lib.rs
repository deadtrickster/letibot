//! A terminal screen, modelled: the bytes a full-screen program writes, applied to a grid a head
//! can read.
//!
//! A pane that shows a full-screen program needs a session the program keeps running in and a screen
//! to draw it into. This crate is the second, and it is deliberately the half that needs nothing
//! else: `Screen::feed` takes bytes, `Screen::rows` gives cells back, and no file descriptor, thread,
//! clock or session appears anywhere in it. That is what makes every test here a `feed` and an
//! assertion.
//!
//! A head reads bytes from the program, feeds them to the screen, and draws the rows:
//!
//! ```text
//!     read bytes from the program ──▶ Screen::feed ──▶ rows()/line() ──▶ a frame
//! ```
//!
//! Two facts it needs from the model rather than from the program:
//!
//! - [`Screen::alternate`]: the alternate screen is up, so the pane is drawing a program and the
//!   transcript is parked behind it, intact.
//! - [`Screen::cursor`] and [`Screen::cursor_visible`]: where the program left the cursor and whether
//!   it wants one. Whether the pane shows it is the head's decision, because a pane shows a cursor
//!   when it has the focus and focus is a fact this crate does not have.
//!
//! A cell carries the terminal's own vocabulary, a foreground and background slot, bold, dim,
//! reverse, and nothing here names a meaning. The head's vocabulary is a role table, and the mapping
//! has one definition: [`attr`] reads an `SGR` parameter list into a pen and `letibot_ui::ansi`
//! decides which role a pen is drawn as. That is why `letibot-ui` depends on this crate rather than
//! the other way round, and why the walk is here rather than duplicated.
//!
//! Mouse reporting is recorded but never performed, since this crate has no input path. Bracketed
//! paste, hyperlinks, truecolour and 256-colour fidelity, fonts, underline and italic, and answering
//! a program that asks the terminal a question are all absent. Each is stated where it is dropped,
//! and the summary is [`screen`]'s header.
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
pub use screen::{Cell, HISTORY_ROWS, MouseMode, MouseTracking, Screen};
pub use width::char_width;
