//! A skill or standing note offered to the model, drawn as ONE faint line with its body one click
//! away. The daemon sends it as a hub warning (`harnessd::offer_rows`), so it is never a prompt row.
//!
//! ```text
//! skill offered: rust-skills (0.41) · loaded · click for details
//! note offered: build.md (0.33) · click for details
//! ```

use rano::style::Role;

use crate::ui::transcript::line_cards::{CLICK, Card, WarningKind};

/// The registry entry: the two codes the daemon raises.
pub(crate) const WARNING: WarningKind = WarningKind {
    codes: &["skill_offer", "note_offer"],
    read: offer_card,
};

/// The card for an offer's detail: its first line, which must open with one of the two leads.
/// `None` for any other text, so the popup's title lookup does not claim a stranger's detail.
pub(crate) fn offer_card(detail: &str) -> Option<Card> {
    let head = detail.lines().next()?.trim();
    let lead = ["skill offered", "note offered"]
        .into_iter()
        .find(|l| head.strip_prefix(l).is_some_and(|r| r.starts_with(": ")))?;
    Some(Card::new(
        format!("{head} · {CLICK}"),
        0,
        lead,
        Role::Faint,
        head.to_string(),
    ))
}
