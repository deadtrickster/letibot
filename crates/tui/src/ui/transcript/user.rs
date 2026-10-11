//! **A row the person wrote**, as the conversation draws it: their prompt, or a notice spoken in
//! their place.

use crate::ui::render::trim_to;
use crate::ui::transcript::job_card::compact_notice;
use crate::ui::transcript::line_cards::compact_cards;
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::{TranscriptItem, UserPart};

pub(crate) fn user_row_lines(
    it: &SnapshotItem,
    item: &TranscriptItem,
    ctx: &ItemCtx<'_>,
) -> (RowClass, Vec<String>) {
    let ItemCtx {
        cfg,
        subagents,
        job_tails,
        ..
    } = *ctx;
    let TranscriptItem::User { parts, speaker } = item else {
        unreachable!("user_row_lines is handed only User rows");
    };
    let text = user_text(parts);
    // **The operator's own words, sanitised like everybody else's.** Not because
    // they are distrusted but because the *paste* is the risk: a control byte
    // copied out of a terminal, a log or a file arrives here as theirs, and
    // rendered straight it is an instruction to the terminal they are reading
    // it on (§3.1). Their own keystrokes cannot contain one — the decoder hands
    // back `Key::Char` — so nothing a person typed is changed by this.
    //
    // **And the two speakers are two renderings** (R42). A row this session appended
    // — a job completion, a salvage notice, a steering line — is drawn as the
    // session's, not as the person's; see [`session_block`]. The class is `Other`
    // rather than `Speech` for the same reason it is not the block: it is not
    // somebody in the conversation speaking, and the separator it draws around
    // itself should say so.
    match speaker {
        letibot_transcript::Speaker::Operator => (RowClass::Speech, user_block(&text, it.ts, cfg)),
        letibot_transcript::Speaker::Agent => {
            // **A job's completion notice is two lines a job** — the operator: *"i dont need
            // job completion card that huge - first line is j<id> - slug - truncated cmd and
            // second line is truncated output"*. The whole card is a click away (the notice
            // popup); see `job_card` for the shape and for where the output line comes from.
            let cols = session_text_cols(it.ts, cfg);
            // **A red merge target is ONE line** — the operator, of the `EMERGENCY —` notice and
            // the warning above it: *"please make all this: … a one-line similar to job - with a
            // popup"*. **And so is a review's outcome** — the operator, of a reviewer that could
            // not decide, drawn as dozens of lines: *"same story about reviews - … one line +
            // popup"*. The whole notice is the same click away the job card's is; see
            // `line_cards`, `red_card` and `review_card`.
            if let Some(lines) = compact_cards(&text, subagents, job_tails, cols) {
                return (RowClass::Other, session_card_block(&lines, it.ts, cfg));
            }
            if let Some(lines) = compact_notice(&text, subagents, job_tails, cols) {
                return (
                    RowClass::Other,
                    session_block(&lines.join("\n"), it.ts, cfg),
                );
            }
            // **A completion notice is folded; anything else is drawn as it arrived.**
            // The row this replaces carried R7's promise to the model in the operator's
            // reading line — see [`folded_notice`], which folds only what it can
            // account for completely and hands back everything else untouched.
            match folded_notice(&text, subagents) {
                // **A settlement line is a LINE, however long the fact in it is.** One of
                // the facts `folded_notice` folds in is the child's own task, and a head
                // that starts subagents with a brief has tasks of thousands of
                // characters: pasted into the row they drew five wrapped rows of
                // somebody's instructions. The operator, looking at a finished child:
                // *"a giant prompt"*. So every folded line is trimmed to the width the
                // block will draw it in, with the head's own `…` — the rule every other
                // clamped row here keeps, and the one the pane already keeps (its rows
                // end in `trim_to`). What is dropped is on the pane and in the child's
                // own session, which is where a reader goes for the whole of it.
                Some(folded) => {
                    let folded = folded
                        .lines()
                        .map(|l| trim_to(l, cols))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (RowClass::Other, session_block(&folded, it.ts, cfg))
                }
                None => (RowClass::Other, session_block(&text, it.ts, cfg)),
            }
        }
    }
}

/// **A user row's text, as one string** — the parts joined the way the row draws them. The
/// transcript walk and the notice click map read the same words through this.
pub(crate) fn user_text(parts: &[UserPart]) -> String {
    parts
        .iter()
        .map(|p| match p {
            UserPart::Text { text } => text.clone(),
            UserPart::Image { media_type, .. } => format!("[image {media_type}]"),
            UserPart::FileRef { path, .. } => format!("[file {path}]"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// **The text of a row the harness spoke in the operator's place**, or `None` for any other row
/// — the rows a completion notice can be.
pub(crate) fn agent_row_text(it: &SnapshotItem) -> Option<String> {
    match it.item.as_ref()? {
        TranscriptItem::User {
            parts,
            speaker: letibot_transcript::Speaker::Agent,
        } => Some(user_text(parts)),
        _ => None,
    }
}
