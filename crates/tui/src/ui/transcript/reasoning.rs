//! The model's working-out, dimmed and folded.

use crate::app::*;
use crate::ui::render::{BlockCache, Decor, RenderConfig, visible_width};
use crate::ui::*;
use letibot_transcript::TranscriptItem;
use letibot_ui::text::without_control_lines;
use rano::agent::card;
use rano::markdown::IncrementalMarkdown;
use rano::style::Role;

/// The width the reasoning body wraps to, and the style it wraps inside. One place, because two
/// call sites computing it and one forgetting the step makes the block a row taller than the space
/// reserved for it, which moves everything below it every frame.
pub(crate) fn reasoning_cfg(cfg: &RenderConfig) -> RenderConfig {
    let mut r = cfg.inside(Role::Reasoning);
    r.width = reasoning_body_width(cfg.width);
    r
}

/// The columns a reasoning body is actually drawn in - the frame's width, less the step it is set
/// in and the rail every row carries. One definition, because the body's wrap and the count of what
/// it costs must be the same arithmetic, or the count reads short of what opening it shows.
pub(crate) fn reasoning_body_width(frame: usize) -> usize {
    frame
        .saturating_sub(activity_indent(frame) + card::REASONING_RAIL_WIDTH)
        .max(20)
}

/// The rail every row of the model's reasoning carries. Three signals, because each is lost
/// somewhere: the word, the rail, and the dim-italic attribute - colour dies under a terminal's own
/// palette, the rail survives a copy-paste, and the attribute survives a `--replay` diff.
pub(crate) fn reasoning_decor(cfg: &RenderConfig) -> Decor {
    let p = cfg.palette();
    // The step the whole of the model's working is set in, carried on the same
    // prefix as the rail so it is applied once per line as the line enters the
    // cache - not once per line per frame, which is what the decor exists for.
    let step = " ".repeat(activity_indent(cfg.width));
    // The rail is painted inside the block too, so a row obeys one invariant end to end: every reset
    // either ends the row or hands the reasoning style straight back. An invariant with an exception
    // at column 0 is one nobody can check; the cost is the opening sequence twice, which collapses.
    let register = rano::render::Style::of(Role::Reasoning);
    let rail = rano::render::Line {
        spans: vec![rano::render::Span::role("┃", Role::Faint)],
        style: register.clone(),
    };
    Decor {
        prefix: format!("{step}{} ", rail.to_ansi_inside(p)),
        open: register.look(p).sgr(),
    }
}

/// How many display lines a reasoning block is - wrapped, not counted by newline. One implementation
/// with two callers, because the row's header and the run marker must not disagree about the same
/// block. The width is the FRAME's; the step and the rail come off inside, as the body's wrap does.
pub(crate) fn reasoning_display_lines(text: &str, w: usize) -> usize {
    let w = reasoning_body_width(w);
    let text = without_control_lines(text);
    text.lines()
        .map(|l| visible_width(l).div_ceil(w).max(1))
        .sum::<usize>()
        .max(1)
}

/// The fold's own header, which is also where its key is advertised: there is no pointer here and no
/// selection, so naming the key is the whole discoverability mechanism. The count is SCREEN lines,
/// because the model writes a few very long paragraphs - `3 lines` beside half a screen is a lie.
pub(crate) fn thinking_header(
    cfg: &RenderConfig,
    raw: &str,
    open: bool,
    running: bool,
    elapsed_ms: Option<u64>,
) -> String {
    let w = cfg.width.max(20);
    let lines = reasoning_display_lines(raw, cfg.width);
    let mark = if open { "▾" } else { "▸" };
    let mut header = rano::render::Line::raw(format!("{mark} "));
    if let Some(word) = card::reasoning(&[], running, elapsed_ms, &card_cfg(cfg, Fold::Folded))
        .into_iter()
        .next()
    {
        header.spans.extend(word.spans);
    }
    header.push(rano::render::Span::role(
        format!(
            " · {lines} line{} · ctrl-r",
            if lines == 1 { "" } else { "s" }
        ),
        Role::Faint,
    ));
    crate::ui::render::row(
        &rano::render::text::truncate_owned(header, w),
        cfg.palette(),
    )
}

pub(crate) fn reasoning_row_lines(
    item: &TranscriptItem,
    ctx: &ItemCtx<'_>,
    ind: usize,
) -> (RowClass, Vec<String>) {
    let ItemCtx { cfg, think, .. } = *ctx;
    let TranscriptItem::Reasoning { text, .. } = item else {
        unreachable!("reasoning_row_lines is handed only Reasoning rows");
    };
    // The model's reasoning is text this head did not author, and it reaches the terminal through the
    // markdown renderer with the head's own escapes around it - so a control byte in it is a control
    // byte on the glass. Sanitised BEFORE the lexer, so what is lexed and what is measured agree.
    let text = without_control_lines(text);
    // A `Cow`, and the borrow is what the change bought: this used to force the
    // allocation even when the sanitiser had nothing to do.
    let text: &str = &text;
    // A settled row: `Thought`, with no duration. The head can compute one
    // for a *live* turn from the delta timestamps, and a transcript row
    // carries no timestamps at all — see `crates/ui/DESIGN.md` §4.4.
    let mut out = step_in(
        vec![thinking_header(cfg, text, think.is_open(), false, None)],
        ind,
    );
    if think.is_open() {
        let rcfg = reasoning_cfg(cfg);
        let mut md = IncrementalMarkdown::new();
        md.push(text);
        let mut cache = BlockCache::decorated(reasoning_decor(cfg));
        out.extend(cache.lines(&md, &rcfg, cfg.budget.reasoning_lines));
    }
    (RowClass::Activity, out)
}
