//! A row the model wrote: its prose, and a line for each call it made that has no result.

use crate::ui::render::{BlockCache, row};
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use letibot_ui::text::without_control_lines;
use rano::agent::card;
use rano::markdown::IncrementalMarkdown;

pub(crate) fn assistant_row_lines(
    it: &SnapshotItem,
    item: &TranscriptItem,
    ctx: &ItemCtx<'_>,
    ind: usize,
) -> (RowClass, Vec<String>) {
    let ItemCtx {
        cfg,
        raw,
        answered,
        drawn_live,
        vis,
        workspace,
        ..
    } = *ctx;
    let TranscriptItem::Assistant {
        text, tool_calls, ..
    } = item
    else {
        unreachable!("assistant_row_lines is handed only Assistant rows");
    };
    // Model prose is content this head did not author, and `paint_full` writes a row verbatim - so a
    // control byte the model emitted is an instruction to the operator's terminal. Sanitised BEFORE
    // the lexer, so lexer, wrapper and width arithmetic all work on one string.
    let text = without_control_lines(text);
    // The lines it cited, in the fences that name them - before the lexer, so rano draws them
    // as a citation (see `cite`). A row that cites nothing is borrowed straight through.
    let text = crate::ui::transcript::cite::fill(&text, &it.citations, workspace);
    // A `Cow`: the borrow avoids an allocation when the sanitiser had nothing to do.
    let text: &str = &text;
    let mut md = IncrementalMarkdown::new();
    md.push(text);
    let mut cache = BlockCache::new();
    // The answer sits at the body's own column, with the question. It is
    // the one thing on the screen that is not subordinate to something
    // else, and that is what says so.
    let prose = cache.lines(&md, cfg, cfg.budget.body_lines);
    let spoke = !prose.iter().all(|l| l.trim().is_empty());
    let mut out = prose;
    // The pictures the reply's markdown named, drawn where the terminal can show them, and only for
    // what the upload pass has already read and sent. Each goes after the first line, past the
    // previous picture, that shows its reference; one with no reference goes at the end.
    if cfg.images {
        let box_cols = rano::term::graphics::image_box(cfg.width);
        let mut from = 0;
        for (alt, target) in crate::ui::render::markdown_images(text) {
            let Some((id, pw, ph)) = crate::ui::render::reply_image(&it.item_id, &target) else {
                continue;
            };
            let (cols, rows) = rano::term::graphics::image_cells(pw, ph, box_cols);
            let picture = rano::term::graphics::image_rows(id, cols, rows);
            let at =
                crate::ui::render::picture_anchor(&out, from, &alt, &target).unwrap_or(out.len());
            from = at + picture.len();
            out.splice(at..at, picture);
        }
    }
    let mut acted = false;
    let p = cfg.palette();
    // A model's answer is the conversation; the calls it made are the working
    //. The prose above is kept and the call rows below are not, which is the
    // same line the ladder draws everywhere else.
    for c in tool_calls.iter().filter(|_| !vis.hides_the_working()) {
        // ONE ROW PER CALL: a call whose result is on the screen is drawn by that result, not here,
        // because a proposal row plus the settled row is two rows for one fact. What survives is the
        // case the proposal line is for - a call with no result, so the arrow means asked and unanswered.
        if answered.contains(&c.id) || drawn_live {
            if raw && !c.arguments.is_empty() {
                out.extend(raw_call_lines(cfg, &format!("{} {}", c.name, c.arguments)));
            }
            continue;
        }
        let verb = card::Verb::of(&c.name);
        // Derived from the arguments on this row, never looked up by call id: this row holds the
        // bytes the rule reads, and an id-keyed lookup once put a call above a card about another
        // file. It is the engine's own `display_target`, so a live call and a replayed one agree.
        let target = letibot_sessionlog::display_target(&c.arguments);
        let subject = if target.is_empty() {
            // The call id earns its columns only when there is nothing
            // better: it is a correlation key, and it is the only thing
            // that distinguishes two calls to the same tool.
            format!("({})", c.id)
        } else {
            target
        };
        // ` · no result` said out loud, because a row that looks like every
        // other tool row and quietly has no output is the shape a person reads
        // straight past. It is the only thing this row now means.
        acted = true;
        out.push(row(
            &rano::render::text::truncate_owned(
                rano::agent::screens::unanswered_call(verb.label(false), &subject, ind),
                cfg.width,
            ),
            p,
        ));
        // The settled row's half of `ctrl-x`: a live turn shows the raw markup from the `ToolCall`
        // deltas, and once the row is committed only the parsed arguments remain. Different bytes,
        // same question - and saying which one you are looking at is evidence rather than a guess.
        if raw && !c.arguments.is_empty() {
            out.extend(raw_call_lines(cfg, &format!("{} {}", c.name, c.arguments)));
        }
    }
    // A row that says something is speech; a row that only names calls is
    // working. A row that does both is speech, because the sentence is
    // what the reader's eye is going to land on.
    let class = if spoke {
        RowClass::Speech
    } else if acted {
        RowClass::Activity
    } else {
        RowClass::Other
    };
    (class, out)
}
