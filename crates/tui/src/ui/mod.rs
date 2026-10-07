//! **What the head draws**, one file per widget. Every function here reads the app
//! (`crate::app`) and returns rows of text; none of it changes the app's state or talks to
//! the terminal (`crate::backend`).

pub mod cards;
pub mod composer;
pub mod header;
pub mod hint_bar;
pub mod loading;
pub mod markdown;
pub mod paint;
pub mod panes;
pub mod render;
pub mod screen;
pub mod status;
pub mod transcript;
pub mod turn_status;

pub(crate) use cards::decision::*;
pub(crate) use composer::*;
pub(crate) use loading::*;
pub(crate) use paint::*;
pub(crate) use panes::help::*;
pub(crate) use panes::notes::*;
pub(crate) use panes::subagents::*;
pub(crate) use panes::todos::*;
pub(crate) use screen::*;
pub(crate) use transcript::assistant::*;
pub(crate) use transcript::blocks::*;
pub(crate) use transcript::call::*;
pub(crate) use transcript::item::*;
pub(crate) use transcript::markers::*;
pub(crate) use transcript::reasoning::*;
pub(crate) use transcript::tool_result::*;
pub(crate) use transcript::turn::*;
pub(crate) use transcript::user::*;
