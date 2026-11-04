//! What the operator sees of an offer: a warning on the hub, never a transcript row. A row would
//! enter the prompt and cannot be un-said, and the model already has its own tail line.

use crate::{notes_offer, skills_offer};

/// The hub warning codes the head draws as clickable rows.
pub const SKILL_CODE: &str = "skill_offer";
pub const NOTE_CODE: &str = "note_offer";

/// How much of the body rides in the warning. The log keeps every offer, so a whole note would
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

/// Publish a skill offer to the heads. The hub's log keeps it, so a head attaching later has it.
pub fn show_skill(hub: &letibot_sessionlog::hub::Hub, o: &skills_offer::Offer, body: Option<&str>) {
    hub.warn(SKILL_CODE, skill_detail(o, body));
}

/// Publish a note offer to the heads.
pub fn show_note(hub: &letibot_sessionlog::hub::Hub, o: &notes_offer::Offer, text: Option<&str>) {
    hub.warn(NOTE_CODE, note_detail(o, text));
}

#[cfg(test)]
mod tests;
