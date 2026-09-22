//! **Text a head did not author, made safe for a terminal** (§3.1) — re-exported.
//!
//! The implementation lives in `letibot_transcript::sanitize`, because it is needed on
//! **both** sides of the log/wire boundary: `letibot-sessionlog` composes a call's display
//! target out of the model's own arguments, and this crate composes rows. `sessionlog`
//! cannot see this crate and must not be given a reason to, so the one definition sits in
//! the crate both already depend on — the same arrangement, and the same reason, as
//! `ToolEditExcerpt`.
//!
//! This module is the forward: call sites in this crate and in `letibot-tui` keep saying
//! `letibot_ui::text::without_control`, and there is one implementation behind it. See
//! `letibot_transcript::sanitize` for the rule — **guard foreign content where it enters,
//! never a string the head has already composed or painted.**

pub use letibot_transcript::sanitize::{without_control, without_control_lines};
