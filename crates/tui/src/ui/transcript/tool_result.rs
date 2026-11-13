//! A tool's result row: the header, the diff or the payload's window, the decision that gated it,
//! and the picture it returned - drawn by `rano::agent::tool_row` from the transcript row. The row's
//! own rules live there; here is the head's half: the facts mapped on, and a foreign program's bytes.

use crate::ui::render::row_strings;
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use rano::agent::tool_row::ToolRow;

/// The tool whose call is an exchange with a person (`harnessd`'s `ASK_USER_QUESTION`).
const ASK_USER_QUESTION: &str = "ask_user_question";

pub(crate) fn tool_result_row_lines(
    it: &SnapshotItem,
    item: &TranscriptItem,
    ctx: &ItemCtx<'_>,
    newest: bool,
    ind: usize,
) -> (RowClass, Vec<String>) {
    let ItemCtx {
        cfg,
        tools,
        targets,
        elapsed_ms,
        edits: live_edits,
        decision,
        diff_split,
        payload_view,
        payload_max,
        window_rows,
        ..
    } = *ctx;
    let TranscriptItem::ToolResult {
        name,
        outcome,
        payload,
        call_id,
        edits: row_edits,
        origin,
        media,
    } = item
    else {
        unreachable!("tool_result_row_lines is handed only ToolResult rows");
    };
    // The picture itself, where the terminal can draw one (kitty graphics, Unicode placeholders):
    // rows of text the terminal fills with the image the head uploaded under this row's id. PNG only,
    // because the protocol takes it as it is; every other format keeps the line the payload says.
    let picture: Vec<rano::render::Line> = match media {
        Some(m) if cfg.images && m.mime == "image/png" => {
            let (cols, rows) = rano::term::graphics::image_cells(
                m.width,
                m.height,
                rano::term::graphics::image_box(cfg.width),
            );
            rano::term::graphics::image_lines(
                rano::term::graphics::image_id(&it.item_id),
                cols,
                rows,
            )
        }
        _ => Vec::new(),
    };
    // The row's own excerpts when it has them: every row the runtime builds carries the same bounded
    // pairs the event does, so a row read out of a store draws its diffs without this head having
    // watched the call run. The live copy wins when both exist - it is what this head saw.
    let edits: &[letibot_transcript::ToolEditExcerpt] = if live_edits.is_empty() {
        row_edits
    } else {
        live_edits
    };
    let edit = edits.first();
    // The envelope is the model's, so it is noise on a screen. The bytes are the command's, and an
    // escape that reaches the terminal is a mode change nobody asked for - so each line is walked by
    // `letibot_ui::ansi::line`: SGR painted into the palette's roles, everything else removed whole.
    let payload = payload.lines().map(letibot_ui::ansi::line).collect();
    let p = cfg.palette();
    let w = cfg.width.saturating_sub(ind).max(20);
    // A call the person ran says so: a tool row names its call and never its author, which is right
    // for the model - one proposer - and wrong for the person's, so an operator-origin row wears
    // `user_block`'s own bar and role, a GLYPH because under `Palette::None` colour alone says nothing.
    let operator = matches!(
        origin,
        Some(letibot_transcript::CallOrigin::Operator { .. })
    );
    let shown = ctx.cards.verb(name, edit);
    let target = match targets.get(call_id) {
        // An edit card names its file, and a note's target is its stem and its text.
        _ if shown != *name => edit.map(|e| e.path.clone()).unwrap_or_default(),
        Some(t) if !t.is_empty() => t.clone(),
        _ => String::new(),
    };
    let verb = rano::agent::card::Verb::of(&shown);
    // An edit draws its diff, not the tool's prose - when this head holds both sides; a
    // row it did not watch keeps the prose, which is the `Replayed` rule.
    let diff = edit
        .filter(|_| verb.is_an_edit())
        .map(|e| crate::ui::edit_diff(e, w.saturating_sub(2), cfg, diff_split));
    // And a change DETECTED after a command draws one too - below the command's own output rather
    // than instead of it, since the body is what the command SAID and the change is a second thing.
    // The verb is `Ran`, so rano will not draw the panel: [`detected_diff_rows`] is this head's half.
    let detected = (!edits.is_empty()).then(|| {
        detected_diff_rows(
            edits,
            DiffRows {
                target: &target,
                width: w.saturating_sub(2),
                cfg,
                diff_split,
                fold: tools,
                under_a_panel: verb.is_an_edit(),
                provenance: DETECTED_PROVENANCE,
            },
        )
    });
    // The payload's own window: `payload_view` carries *which* row, because several
    // payloads can be unfolded on one screen and a bare offset would page all of them.
    let window = payload_view
        .filter(|(id, _)| *id == it.item_id.as_str())
        .map(|(_, page)| page);
    let row = ToolRow {
        target,
        elapsed_ms,
        operator,
        diff,
        decision: decision.map(crate::ui::settled_decision),
        picture,
        // The file, as a link (OSC 8) where the terminal speaks it: the full target the
        // shortened subject stands for, so a click opens the file and not `…/app.rs`.
        link_root: cfg.links.clone(),
        fold: tools.into(),
        window,
        window_rows,
        body_lines: cfg.budget.body_lines,
        newest,
        // The step is the head's, applied to the strings, outside the row's own register: a payload
        // row is drawn as its dim body with the step in front of it, and a step taken inside the line
        // would put the step under the dim - the same columns, but not the same bytes.
        indent: 0,
        ..ToolRow::new(
            &shown,
            call_id,
            crate::ui::display_outcome(outcome),
            payload,
        )
    };
    let layout = row.layout(cfg.width.saturating_sub(ind));
    if let (Some(max), Some(cell)) = (layout.max_page, payload_max) {
        cell.set(max);
    }
    let mut rendered = row_strings(&layout.lines, p);
    // `… +N lines · /t unfolds it` is rano's, and it names the verb's word for the state the head
    // holds: with the fold already open `/t` takes the rest AWAY, so a row that still elides has to
    // say so. The clause is a word and not a count, so it is rewritten and nothing is re-measured.
    if tools.is_open() {
        for l in &mut rendered {
            if l.contains("· /t unfolds it") {
                *l = l.replace("· /t unfolds it", "· /t folds it");
            }
        }
    }
    // A question's card is its header, folded: every other tool's folded card keeps its first payload
    // line and the seam, because there that line is the only place the result is on the screen - but
    // this one's result is the exchange, which is the `asked` line and the person's answer right below.
    if name == ASK_USER_QUESTION && window.is_none() && !tools.is_open() && rendered.len() > 1 {
        rendered.truncate(1);
    }
    if let Some(rows) = detected {
        rendered.extend(rows);
    }
    (RowClass::Activity, step_in(rendered, ind))
}

/// The lines a FOLDED tool row leaves under its `… +N lines` seam: the marker's own count, and the rule
/// that says which row `ctrl-v` opens. Zero for a fold that keeps everything - one line rides the header,
/// an edit draws its diff, and a failed call with no sentence draws its whole body, `body_lines` of it.
pub(crate) fn folded_rest(it: &SnapshotItem, body_lines: usize) -> usize {
    let Some(TranscriptItem::ToolResult {
        name,
        payload,
        outcome,
        edits,
        ..
    }) = it.item.as_ref()
    else {
        return 0;
    };
    // The diff arm returns before the payload is laid out, so its own seam is the one this row
    // draws: `+N diff rows`, and `/t` is already the only key that reaches them.
    if !edits.is_empty() && rano::agent::card::Verb::of(name).is_an_edit() {
        return 0;
    }
    let shown = display_outcome(outcome);
    let kept = if !matches!(shown, rano::agent::outcome::Outcome::Ok) && shown.reason().is_none() {
        body_lines
    } else {
        1
    };
    // Counted the way rano counts it: envelope markers and a restated reason are gone before
    // either the count or the seam is worked out.
    payload
        .lines()
        .map(rano::agent::text::clean_line)
        .filter(|l| {
            !rano::agent::text::is_envelope(l) && !rano::agent::tool_row::restates_reason(l, &shown)
        })
        .count()
        .saturating_sub(kept)
}
