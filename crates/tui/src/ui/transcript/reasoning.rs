//! **The model's working-out**, dimmed and folded.

use crate::app::*;
use crate::ui::render::{Decor, RenderConfig, trim_to, visible_width};
use crate::ui::*;
use letibot_ui::card;
use letibot_ui::style::{Painter, Role};
use letibot_ui::text::without_control_lines;

/// The rail every line of the model's reasoning carries.
///
/// Three independent signals, because each one is lost somewhere: the **word**
/// (`Thinking…` / `Thought for 4.2s`), the **rail** (`┃`), and the **dim-italic
/// attribute**. Colour is a no-op under a terminal-native palette; the rail is
/// what survives a copy-paste; the attribute is what survives a `--replay` diff.
/// Handing it to the `BlockCache` as a `Decor` rather than mapping over the lines
/// per frame is what keeps §13.3: the rail is applied once, when the line enters
/// the cache, not once per line per frame.
/// The width the reasoning body wraps to, and the style it wraps inside.
///
/// One place, because two call sites computing it and one of them forgetting the
/// step makes the block a row taller than the space reserved for it, which moves
/// everything below it every frame.
pub(crate) fn reasoning_cfg(cfg: &RenderConfig) -> RenderConfig {
    let mut r = cfg.inside(Role::Reasoning);
    r.width = cfg
        .width
        .saturating_sub(activity_indent(cfg.width) + card::REASONING_RAIL_WIDTH)
        .max(20);
    r
}

pub(crate) fn reasoning_decor(cfg: &RenderConfig) -> Decor {
    let p = cfg.palette();
    // The step the whole of the model's working is set in, carried on the same
    // prefix as the rail so it is applied once per line as the line enters the
    // cache — not once per line per frame, which is what §13.3 forbids.
    let step = " ".repeat(activity_indent(cfg.width));
    // The rail is painted **inside** the block too, so that the row obeys one
    // invariant end to end: every reset in a reasoning row either ends the row or
    // hands the reasoning style straight back. That is what the test asserts, and
    // an invariant with an exception at column 0 is an invariant nobody can check.
    // The cost is the block's opening sequence twice at the head of each row,
    // which a terminal collapses to nothing.
    let rail = Painter::inside(p, Role::Reasoning);
    Decor {
        prefix: format!("{step}{} ", rail.paint(Role::Faint, "┃")),
        open: p.open(Role::Reasoning).to_string(),
    }
}

/// **How many display lines a reasoning block is** — wrapped, not counted by newline.
///
/// One implementation with two callers, because R37's marker and the thinking row's own
/// header must not disagree about the same block: the header says `▸ Thought · 43 lines`
/// and the marker says `43 thinking lines`, and a reader who opens the run sees the 43.
/// A long unwrapped line is several display lines and counts as several — the same
/// arithmetic, and the same reason, as `visible_width(...).div_ceil(w)`.
pub(crate) fn reasoning_display_lines(text: &str, w: usize) -> usize {
    let w = w.max(20);
    let text = without_control_lines(text);
    text.lines()
        .map(|l| visible_width(l).div_ceil(w).max(1))
        .sum::<usize>()
        .max(1)
}

/// The fold's own header, which is also where its key is advertised.
///
/// `card::reasoning` supplies the word and the tense; this adds the two things
/// only the head knows — how much of the terminal opening it would cost, and
/// which key opens it. There is no pointer here and no selection, so the fold's
/// own header naming its key is the whole discoverability mechanism, and it is on
/// the screen at the moment the operator wants it.
///
/// The count is **screen** lines, not source lines: the model writes its
/// working-out as a handful of very long paragraphs, so "3 lines" beside a fold
/// that opens to half a screen is a number that answers the wrong question. What
/// the reader wants to know is how much of the terminal this is about to cost.
/// See [`reasoning_display_lines`], which computes it for this header and for R37's marker.
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
    let word = card::reasoning(&[], running, elapsed_ms, &card_cfg(cfg, Fold::Folded))
        .into_iter()
        .next()
        .unwrap_or_default();
    trim_to(
        &format!(
            "{mark} {word}{}",
            dim(
                cfg,
                &format!(
                    " · {lines} line{} · ctrl-r",
                    if lines == 1 { "" } else { "s" }
                )
            )
        ),
        w,
    )
}
