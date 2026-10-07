//! **A row the person wrote**, as the conversation draws it: their prompt, or a notice spoken in
//! their place.

use crate::ui::render::trim_to;
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::{TranscriptItem, UserPart};

pub(crate) fn user_row_lines(
    it: &SnapshotItem,
    item: &TranscriptItem,
    ctx: &ItemCtx<'_>,
) -> (RowClass, Vec<String>) {
    let ItemCtx { cfg, subagents, .. } = *ctx;
    let TranscriptItem::User { parts, speaker } = item else {
        unreachable!("user_row_lines is handed only User rows");
    };
    let text = parts
        .iter()
        .map(|p| match p {
            UserPart::Text { text } => text.clone(),
            UserPart::Image { media_type, .. } => format!("[image {media_type}]"),
            UserPart::FileRef { path, .. } => format!("[file {path}]"),
        })
        .collect::<Vec<_>>()
        .join(" ");
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
                    let cols = session_text_cols(it.ts, cfg);
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
