//! A skill or standing note offered to the model, drawn as ONE faint line with its body one click
//! away. It is a display-only transcript row (`TranscriptItem::Offer`), so every head draws it in
//! place from the record and no renderer sends it to the model.
//!
//! ```text
//! skill offered: rust-skills (0.41) · loaded · click for details
//! note offered: build.md (0.33) · click for details
//! ```

use letibot_transcript::OfferKind;
use rano::style::Role;

use crate::ui::transcript::line_cards::{CLICK, Card};

/// The card for an offer row: its headline, the lead accented, the popup titled by the headline.
pub(crate) fn offer_card(kind: OfferKind, title: &str) -> Card {
    let lead = match kind {
        OfferKind::Skill => "skill offered",
        OfferKind::Note => "note offered",
    };
    let head = title.lines().next().unwrap_or("").trim();
    Card::new(
        format!("{head} · {CLICK}"),
        0,
        lead,
        Role::Faint,
        head.to_string(),
    )
}
