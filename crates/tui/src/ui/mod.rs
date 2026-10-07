//! **What the head draws**, one file per widget. Every function here reads the app
//! (`crate::app`) and returns rows of text; none of it changes the app's state or talks to
//! the terminal (`crate::backend`).

pub mod cards;
pub mod panes;

pub(crate) use cards::decision::*;
pub(crate) use panes::help::*;
pub(crate) use panes::notes::*;
pub(crate) use panes::subagents::*;
pub(crate) use panes::todos::*;
