//! A terminal screen, modelled: the bytes a full-screen program writes, applied to a grid a head
//! can read.
//!
//! Deliberately self-contained: `Screen::feed` takes bytes and `Screen::rows` gives cells back, with
//! no file descriptor, thread, clock or session, so every test is a `feed` and an assertion.
//!
//! ```text
//!     read bytes from the program ──▶ Screen::feed ──▶ rows()/line() ──▶ a frame
//! ```
//!
//! A cell carries the terminal's vocabulary only; [`attr`] reads `SGR` into a pen and
//! `letibot_ui::ansi` picks the role, which is why `letibot-ui` depends on this crate. Mouse
//! reporting is recorded, never performed; what else is dropped is listed in [`screen`]'s header.

pub mod attr;
pub mod parser;
pub mod screen;
pub mod width;

pub use attr::{Attr, Hue, apply_sgr};
pub use parser::{Csi, Event, MAX_PARAMS, Parser};
pub use screen::{Cell, HISTORY_ROWS, MouseMode, MouseTracking, Screen};
pub use width::char_width;
