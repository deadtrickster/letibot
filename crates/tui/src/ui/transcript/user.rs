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
        replied_to,
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
        letibot_transcript::Speaker::Operator => {
            let mut rows = user_block(&text, it.ts, cfg);
            // **The pictures this prompt attached, under their chips** — the operator, from a
            // fresh session with an image pasted and sent: *"image doesnt render"*. The chip in
            // their own words names it (`[image 1 · png · 195 KiB]`), and the picture itself is
            // drawn below the block as rows of kitty placeholders the terminal fills from the
            // bytes the upload pass sent (`app::attention::queue_image_uploads`).
            //
            // **Where the terminal takes no picture the chip is the whole of it**, which is why
            // this is a `cfg.images` arm and nothing else: a head that wrote placeholders to a
            // terminal that does not fill them would draw a row of invisible tofu.
            //
            // Every picture is drawn in the order the row carries them, so two of them are two
            // thumbnails and not one.
            if cfg.images {
                rows.extend(user_pictures(&it.item_id, parts, cfg));
            }
            // **An answer names the question it answers** — the operator: *"my reply doesnt
            // have any hint on the question. lets mention the question i replied to"*. A faint
            // line under their words, which stay verbatim (the trail cites them); see
            // `asked_card`.
            if let Some(re) = replied_to.and_then(crate::ui::transcript::asked_card::replied_to) {
                rows.extend(reply_note(&re, cfg));
            }
            (RowClass::Speech, rows)
        }
        letibot_transcript::Speaker::Agent => (
            RowClass::Other,
            session_row_lines(&text, it.ts, cfg, subagents, job_tails),
        ),
    }
}

/// **A row this session spoke in the operator's place, drawn** — the agent arm of
/// [`user_row_lines`], on its own so the notice measurement (`walls`) reads exactly what the
/// transcript draws.
pub(crate) fn session_row_lines(
    text: &str,
    ts: u64,
    cfg: &crate::ui::render::RenderConfig,
    subagents: &[crate::app::SubagentState],
    job_tails: &std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
) -> Vec<String> {
    // **A job's completion notice is two lines a job** — the operator: *"i dont need
    // job completion card that huge - first line is j<id> - slug - truncated cmd and
    // second line is truncated output"*. The whole card is a click away (the notice
    // popup); see `job_card` for the shape and for where the output line comes from.
    let cols = session_text_cols(ts, cfg);
    // **A red merge target is ONE line** — the operator, of the `EMERGENCY —` notice and
    // the warning above it: *"please make all this: … a one-line similar to job - with a
    // popup"*. **And so is a review's outcome** — the operator, of a reviewer that could
    // not decide, drawn as dozens of lines: *"same story about reviews - … one line +
    // popup"*. The whole notice is the same click away the job card's is; see
    // `line_cards`, `red_card` and `review_card`.
    if let Some(lines) = compact_cards(text, subagents, job_tails, cols) {
        return session_card_block(&lines, ts, cfg);
    }
    if let Some(lines) = compact_notice(text, subagents, job_tails, cols) {
        return session_block(&lines.join("\n"), ts, cfg);
    }
    // **A completion notice is folded; anything else is drawn as it arrived.**
    // The row this replaces carried R7's promise to the model in the operator's
    // reading line — see [`folded_notice`], which folds only what it can
    // account for completely and hands back everything else untouched.
    match folded_notice(text, subagents) {
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
            session_block(&folded, ts, cfg)
        }
        // **And anything else is ONE line a notice** (R89) — the operator's standing rule, *"make
        // them the usual one liners"*. A row no card and no fold accounts for used to be drawn as
        // it arrived: a plan check, a parent's brief, a length cap's 6,902 lines. Each notice in
        // it is now a line (`notice_card`), the whole row a click away.
        None => session_card_block(&notice_lines(text, subagents, job_tails, cols), ts, cfg),
    }
}

/// **A row's notices, one line each** — a card's, a job's two, a fold's, or `notice_card`'s
/// line for a notice nothing else reads. Never empty, never a wall.
pub(crate) fn notice_lines(
    text: &str,
    subagents: &[crate::app::SubagentState],
    job_tails: &std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
    cols: usize,
) -> Vec<crate::ui::transcript::line_cards::CardLine> {
    use crate::ui::transcript::line_cards::CardLine;
    use crate::ui::transcript::notice_card::{notices, session_notice};
    let mut out = Vec::new();
    for part in notices(text) {
        if let Some(lines) = compact_cards(part, subagents, job_tails, cols) {
            out.extend(lines);
        } else if let Some(lines) = compact_notice(part, subagents, job_tails, cols) {
            out.extend(lines.into_iter().map(CardLine::plain));
        } else if let Some(folded) = folded_notice(part, subagents) {
            out.extend(folded.lines().map(|l| CardLine::plain(trim_to(l, cols))));
        } else {
            out.push(session_notice(part).cut(cols));
        }
    }
    out
}

/// **A user row's thumbnails, as the rows they are drawn in**: the picture's placeholder rows,
/// cut to the box the width gives it and set in under the operator's block.
///
/// The rows are [`rano::term::graphics::image_rows`] — the same ones a tool result's picture is
/// drawn with, and the same cluster the terminal reads its placement out of. Nothing is read here:
/// a part whose bytes the upload pass has not reached yet draws nothing this frame, and the pass
/// invalidates the history when it does.
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

/// **The faint `re: "…"` line under an operator's answer**, behind the same `▌` bar their
/// words wear, cut to the width with the head's `…`.
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

/// **A user row's text, as the row draws it** — the operator's words, and not what an attachment
/// added beside them. The transcript walk and the notice click map read the same words through
/// this.
///
/// **The first `Text` part is the message; the ones after it are the attachments'.** A prompt's
/// parts are the words first and then what each chip became (`PROTOCOL_VERSION` 50, and
/// `harnessd::attachments` states the reason the words stay whole and first): a picture's bytes
/// and a text file's inlined content ride in parts of their own, and the chip in the operator's
/// own sentence — `[image 1 · png · 92 KiB]` — is what names it on the screen. Drawing the parts
/// after the first would put a 30 KB CSV into the conversation under a question about it, and
/// drawing a part as well as the chip would make one attachment look like two.
///
/// A row with no words at all still says what it carries, for a producer that ever writes one.
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
