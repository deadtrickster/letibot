//! **A row the model wrote**: its prose, and a line for each call it made that has no result.

use crate::ui::markdown::IncrementalMarkdown;
use crate::ui::render::{BlockCache, trim_to};
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use letibot_ui::card;
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;

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
        ..
    } = *ctx;
    let TranscriptItem::Assistant {
        text, tool_calls, ..
    } = item
    else {
        unreachable!("assistant_row_lines is handed only Assistant rows");
    };
    // **Model prose is content this head did not author** (§3.1). It is the
    // largest unsanitised surface in this file: it goes through the markdown
    // lexer and comes out as rows the head paints, and `paint_full` writes a row
    // verbatim — so a control byte the model emitted (or copied out of a file it
    // read) was an instruction to the operator's terminal. `ESC ] 0 ; … BEL` sets
    // the window title and `ESC [ 2 J` clears the screen.
    //
    // Sanitised **before** the lexer rather than after the renderer, so the
    // lexer, the wrapper and the width arithmetic all work on one string: a
    // control byte becomes a space, which keeps every column count the same and
    // is the same trade `without_control`'s doc records for tool payloads.
    let text = without_control_lines(text);
    // A `Cow`, and the borrow is what the change bought: this used to force the
    // allocation even when the sanitiser had nothing to do.
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
    // **The pictures the reply's markdown named**, under it, where the terminal can
    // draw them — the operator's first test of inline images was a model that wrote a
    // PNG and said `![sunset](/Users/dead/sunset.png)`, and the head drew the alt text.
    // Only what the upload pass has already read and sent; see `render::reply_image`.
    //
    // **At the reference, not at the end** — the operator, looking at the first one: the
    // `![…]` line was near the top and the picture below the reply's last paragraph.
    // Each goes after the first line, past the previous picture, that shows its
    // reference; one whose reference is on no line goes at the end.
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
    // **A model's answer is the conversation; the calls it made are the working**
    // (R37). The prose above is kept and the call rows below are not, which is the
    // same line the ladder draws everywhere else.
    for c in tool_calls.iter().filter(|_| !vis.hides_the_working()) {
        // ONE ROW PER CALL. A call whose result is on the screen is drawn
        // by that result and not here.
        //
        // This row used to draw `→ Read foo.rs` for every call it made and
        // the result row then drew `▸ Read foo.rs · ok · 21 lines` for the
        // same call three lines below, which is two rows and one fact: the
        // proposal says a call is coming, and once the result has settled
        // nothing is coming. That doubling is most of why a turn read as a
        // wall — four calls cost eight rows of a thirty-four-row screen
        // before any output was shown.
        //
        // What survives is the case the proposal line is actually FOR: a
        // call with no result. The turn was interrupted, the round is still
        // running, or the body has not arrived. `→` now means exactly
        // "asked for, nothing came back", which is a fact worth a row.
        if answered.contains(&c.id) || drawn_live {
            if raw && !c.arguments.is_empty() {
                out.extend(raw_call_lines(cfg, &format!("{} {}", c.name, c.arguments)));
            }
            continue;
        }
        let verb = card::Verb::of(&c.name);
        let mut line = format!("→ {}", verb.label(false));
        // Derived from the arguments **on this row**, never looked up by
        // call id. The row is holding the very bytes the rule reads, and it
        // is the only copy of them that is guaranteed to belong to this
        // round — an id-keyed lookup was how `→ Read TODO.md` came to sit
        // above a card whose payload was `README.md`.
        //
        // It is the same function the engine puts on the wire,
        // `letibot_sessionlog::display_target`, so a call watched live and
        // one reconstructed from the transcript still render identically;
        // a second copy of the rule here is what would make a switched head
        // disagree with the head it switched away from.
        let target = letibot_sessionlog::display_target(&c.arguments);
        if target.is_empty() {
            // The call id earns its columns only when there is nothing
            // better: it is a correlation key, and it is the only thing
            // that distinguishes two calls to the same tool.
            line.push_str(&format!(" ({})", c.id));
        } else {
            line.push(' ');
            line.push_str(&target);
        }
        // Said out loud, because a row that looks like every other tool row
        // and quietly has no output is the shape a person reads straight
        // past. It is the only thing this row now means.
        line.push_str(" · no result");
        acted = true;
        out.push(trim_to(
            &format!("{}{}", " ".repeat(ind), p.paint(Role::Attention, &line)),
            cfg.width,
        ));
        // The settled row's half of `ctrl-x`. A live turn shows the raw
        // markup from the `ToolCall` deltas; once the row is committed the
        // markup is gone and the arguments the parser read out of it are
        // what remain, so that is what the chord shows here. Different
        // bytes, same question — and saying which one you are looking at is
        // the difference between evidence and a guess.
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
