//! What the operator sees of an offer: a display-only `Offer` row in the transcript. Every
//! renderer skips it, so the model keeps only its own tail line.

use letibot_transcript::{OfferKind, TranscriptItem};

use crate::{notes_offer, skills_offer};

/// How much of the body rides in the row. The store keeps every offer, so a whole note would
/// grow it by the note each round.
const BODY_MAX: usize = 6_000;

fn state(taken: bool, word: &str) -> String {
    match taken {
        true => format!(" · {word}"),
        false => String::new(),
    }
}

fn with_body(head: String, body: Option<&str>) -> String {
    let Some(body) = body.filter(|b| !b.trim().is_empty()) else {
        return head;
    };
    let cut = (0..=BODY_MAX.min(body.len()))
        .rev()
        .find(|&i| body.is_char_boundary(i))
        .unwrap_or(0);
    let more = if cut < body.len() { "\n[cut]" } else { "" };
    format!("{head}\n\n{}{more}", &body[..cut])
}

/// The headline line, then the skill's body for the click.
pub fn skill_detail(o: &skills_offer::Offer, body: Option<&str>) -> String {
    let head = format!(
        "skill offered: {} ({:.2}){}",
        o.name,
        o.score,
        state(o.taken, "loaded")
    );
    with_body(head, body)
}

/// The headline line, then the note's path and text for the click.
pub fn note_detail(o: &notes_offer::Offer, text: Option<&str>) -> String {
    let title = o
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| o.path.display().to_string());
    let head = format!(
        "note offered: {title} ({:.2}){}",
        o.score,
        state(o.taken, "read")
    );
    with_body(
        head,
        text.map(|t| format!("{}\n\n{t}", o.path.display()))
            .as_deref(),
    )
}

/// The row for `detail`: the headline, then the body a click opens.
fn row(kind: OfferKind, detail: String) -> TranscriptItem {
    let (title, body) = detail.split_once("\n\n").unwrap_or((&detail, ""));
    TranscriptItem::Offer {
        kind,
        title: title.to_string(),
        body: body.to_string(),
    }
}

/// The transcript row for a skill offer.
pub fn skill_row(o: &skills_offer::Offer, body: Option<&str>) -> TranscriptItem {
    row(OfferKind::Skill, skill_detail(o, body))
}

/// The transcript row for a note offer.
pub fn note_row(o: &notes_offer::Offer, text: Option<&str>) -> TranscriptItem {
    row(OfferKind::Note, note_detail(o, text))
}

#[cfg(test)]
mod tests;
