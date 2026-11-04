//! Text a head did not author, made safe for a terminal - re-exported from `letibot_transcript::sanitize`,
//! which `letibot-sessionlog` also needs and which cannot see this crate.
//! Guard foreign content where it enters, never the head's own.

pub use letibot_transcript::sanitize::{without_control, without_control_lines};
