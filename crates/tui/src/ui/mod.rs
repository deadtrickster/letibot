//! **What the head draws**, one file per widget. Every function here reads the app
//! (`crate::app`) and returns rows of text; none of it changes the app's state or talks to
//! the terminal (`crate::backend`).

pub mod cards;
pub mod paint;
pub mod panes;
pub mod transcript;

pub(crate) use cards::decision::*;
pub(crate) use paint::*;
pub(crate) use panes::help::*;
pub(crate) use panes::notes::*;
pub(crate) use panes::subagents::*;
pub(crate) use panes::todos::*;
pub(crate) use transcript::blocks::*;
pub(crate) use transcript::call::*;
pub(crate) use transcript::item::*;
pub(crate) use transcript::markers::*;
pub(crate) use transcript::reasoning::*;
pub(crate) use transcript::turn::*;
