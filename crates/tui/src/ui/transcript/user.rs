//! A row the person wrote, as the conversation draws it: their prompt, or a notice spoken in their
//! place.

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
        replied_to,
        ..
    } = *ctx;
    let TranscriptItem::User { parts, speaker } = item else {
        unreachable!("user_row_lines is handed only User rows");
    };
    let text = user_text(parts);
    // The operator's own words, sanitised like everybody else's - not because they are distrusted
    // but because the paste is the risk: a control byte copied out of a terminal would be rendered
    // as an instruction to the terminal they read it on. The session's rows are `Other`, not speech.
    match speaker {
        letibot_transcript::Speaker::Operator => {
            let mut rows = user_block(&text, it.ts, cfg);
            // The pictures this prompt attached, under their chips: the chip in their own words
            // names it, and the picture is drawn below the block as rows of kitty placeholders the
            // terminal fills from the bytes the upload pass sent.
            if cfg.images {
                rows.extend(user_pictures(&it.item_id, parts, cfg));
            }
            // An answer names the question it answers: a faint line under their words, which stay
            // verbatim (the trail cites them); see `asked_card`.
            if let Some(re) = replied_to.and_then(crate::ui::transcript::asked_card::replied_to) {
                rows.extend(reply_note(&re, cfg));
            }
            (RowClass::Speech, rows)
        }
        letibot_transcript::Speaker::Agent => (
            RowClass::Other,
            session_row_lines(&text, it.ts, cfg, subagents, job_tails, ctx.cards),
        ),
    }
}

/// A row this session spoke in the operator's place, drawn - the agent arm of [`user_row_lines`],
/// on its own so the notice measurement (`walls`) reads exactly what the transcript draws.
pub(crate) fn session_row_lines(
    text: &str,
    ts: u64,
    cfg: &crate::ui::render::RenderConfig,
    subagents: &[crate::app::SubagentState],
    job_tails: &std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
    cards: &crate::app::Cards<'_>,
) -> Vec<String> {
    session_row(text, ts, cfg, subagents, job_tails, cards).0
}

/// The child each drawn line of a session row is about: one entry per line [`session_row_lines`]
/// draws, `Some(id)` where that line is a finished subagent's settlement. A click on such a line goes
/// to that child, so the two lists must be one drawing - [`session_row`] hands back both.
pub(crate) fn session_row_children(
    text: &str,
    ts: u64,
    cfg: &crate::ui::render::RenderConfig,
    subagents: &[crate::app::SubagentState],
    job_tails: &std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
    cards: &crate::app::Cards<'_>,
) -> Vec<Option<String>> {
    session_row(text, ts, cfg, subagents, job_tails, cards).1
}

/// A session row drawn, and who each line of it is about - the one drawing behind
/// [`session_row_lines`] and [`session_row_children`].
fn session_row(
    text: &str,
    ts: u64,
    cfg: &crate::ui::render::RenderConfig,
    subagents: &[crate::app::SubagentState],
    job_tails: &std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
    cards: &crate::app::Cards<'_>,
) -> (Vec<String>, Vec<Option<String>>) {
    // A job's completion notice is two lines a job, and the whole card is a click away (the notice
    // popup); see `job_card` for the shape and for where the output line comes from.
    let cols = session_text_cols(ts, cfg);
    // A red merge target is ONE line, and so is a review's outcome: a wall of text is a wall
    // whatever it says. The whole notice is the same click away the job card's is; see `line_cards`,
    // `red_card` and `review_card`.
    if let Some(lines) = compact_cards(text, subagents, job_tails, cols) {
        let none = vec![None; lines.len()];
        return (session_card_block(&lines, ts, cfg), none);
    }
    if let Some(lines) = compact_notice(text, subagents, job_tails, cols) {
        let none = vec![None; lines.len()];
        return (session_block(&lines.join("\n"), ts, cfg), none);
    }
    // A completion notice is folded; anything else is drawn as it arrived. The fold is
    // [`folded_notice`]'s, which folds only what it can account for completely and hands everything
    // else back untouched.
    match folded_notice_rows(text, subagents) {
        // A settlement line is a LINE, however long the fact in it is: a child's brief can run to
        // thousands of characters, and pasted in it drew five wrapped rows of somebody's instructions.
        // Every folded line is trimmed with the head's own `...`; the whole is a click away.
        Some(folded) => {
            let children = folded.iter().map(|(_, c)| c.clone()).collect();
            let folded = folded
                .into_iter()
                .map(|(l, _)| trim_to(&l, cols))
                .collect::<Vec<_>>()
                .join("\n");
            (session_block(&folded, ts, cfg), children)
        }
        // And anything else is ONE line a notice: a row no card and no fold accounts for would be
        // drawn as it arrived, a plan check or a parent's brief at six thousand lines. Each notice in
        // it is a line (`notice_card`), the whole row a click away.
        None => {
            let parts = notice_parts(text, subagents, job_tails, cards, cols);
            let children = parts.iter().map(|(_, c)| c.clone()).collect();
            let lines = parts.into_iter().map(|(l, _)| l).collect::<Vec<_>>();
            (session_card_block(&lines, ts, cfg), children)
        }
    }
}

/// A row's notices, one line each - a card's, a job's two, a fold's, or `notice_card`'s line for a
/// notice nothing else reads - with the child each line is about, since a `[task]` group beside a
/// plan check is still a settlement whose line is a door to that child. Never empty, never a wall.
fn notice_parts(
    text: &str,
    subagents: &[crate::app::SubagentState],
    job_tails: &std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
    cards: &crate::app::Cards<'_>,
    cols: usize,
) -> Vec<(crate::ui::transcript::line_cards::CardLine, Option<String>)> {
    use crate::ui::transcript::line_cards::CardLine;
    use crate::ui::transcript::notice_card::notices;
    // The cards asked for ONCE for the row, one per part: the layer's answer where it has one, and
    // the readers - the same functions the built-in layer is built from - where it does not.
    let parts = notices(text);
    let mut drawn = cards.lines(&parts).into_iter();
    let mut out = Vec::new();
    for part in parts {
        let card = drawn
            .next()
            .unwrap_or_else(|| crate::ui::transcript::notice_card::session_notice(part));
        if let Some(lines) = compact_cards(part, subagents, job_tails, cols) {
            out.extend(lines.into_iter().map(|l| (l, None)));
        } else if let Some(lines) = compact_notice(part, subagents, job_tails, cols) {
            out.extend(lines.into_iter().map(|l| (CardLine::plain(l), None)));
        } else if let Some(folded) = folded_notice_rows(part, subagents) {
            out.extend(
                folded
                    .into_iter()
                    .map(|(l, c)| (CardLine::plain(trim_to(&l, cols)), c)),
            );
        } else {
            out.push((card.cut(cols), None));
        }
    }
    out
}

/// A user row's thumbnails, as the rows they are drawn in: the picture's placeholder rows, cut to the
/// box the width gives it and set in under the operator's block. Nothing is read here - a part the
/// upload pass has not reached draws nothing, and the pass invalidates the history when it does.
pub(crate) fn user_pictures(
    item_id: &str,
    parts: &[UserPart],
    cfg: &crate::ui::render::RenderConfig,
) -> Vec<String> {
    let mut out = Vec::new();
    for (k, p) in parts.iter().enumerate() {
        if !matches!(p, UserPart::Image { .. }) {
            continue;
        }
        let Some((id, cols, rows, _)) = crate::ui::render::user_picture_box(item_id, k, cfg.width)
        else {
            continue;
        };
        let inset = "  ";
        out.extend(
            rano::term::graphics::image_rows(id, cols, rows)
                .into_iter()
                .map(|r| format!("{inset}{r}")),
        );
    }
    out
}

/// The faint `re: "..."` line under an operator's answer, behind the same `|` bar their words wear,
/// cut to the width with the head's `...`.
fn reply_note(re: &str, cfg: &crate::ui::render::RenderConfig) -> Vec<String> {
    use rano::render::{Line, Span};
    use rano::style::Role;
    let w = cfg.width.max(20).saturating_sub(2);
    let line = Line::new(vec![
        Span::role("▌", Role::UserAccent),
        Span::raw(" "),
        Span::role(trim_to(re, w), Role::Faint),
    ]);
    crate::ui::render::row_strings(&[line], cfg.palette())
}

/// A user row's text, as the row draws it: the operator's words, and not what an attachment added
/// beside them. The first `Text` part is the message and the ones after it are the attachments', so
/// drawing those would put a 30 KB CSV under the question it was attached to.
pub(crate) fn user_text(parts: &[UserPart]) -> String {
    if let Some(words) = parts.iter().find_map(|p| match p {
        UserPart::Text { text } => Some(text.clone()),
        _ => None,
    }) {
        return words;
    }
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

/// The text of a row the harness spoke in the operator's place, or `None` for any other row - the
/// rows a completion notice can be.
pub(crate) fn agent_row_text(it: &SnapshotItem) -> Option<String> {
    match it.item.as_ref()? {
        TranscriptItem::User {
            parts,
            speaker: letibot_transcript::Speaker::Agent,
        } => Some(user_text(parts)),
        // A delivery is drawn by the same card walk, so it opens the same way.
        TranscriptItem::Notice { text, .. } => Some(text.clone()),
        _ => None,
    }
}
