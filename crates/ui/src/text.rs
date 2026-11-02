//! Text a head did not author, made safe for a terminal - re-exported.
//!
//! The implementation lives in `letibot_transcript::sanitize`, because it is needed on both
//! sides of the log/wire boundary: `letibot-sessionlog` composes a call's display target and
//! this crate composes rows, and `sessionlog` cannot see this crate.
//!
//! This module is the forward, so call sites keep saying `letibot_ui::text::without_control`
//! with one implementation behind it. Guard foreign content where it enters, never the head's own.

pub use letibot_transcript::sanitize::{without_control, without_control_lines};
