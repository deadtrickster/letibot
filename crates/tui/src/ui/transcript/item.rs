//! **One row of the conversation, drawn**: `item_lines` turns a transcript item into its
//! lines, under the visibility, folds and budgets the frame was asked for.

use crate::app::*;
use crate::ui::render::{RenderConfig, trim_to, wrap};
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use letibot_ui::text::without_control_lines;
use rano::agent::card;

/// A `card::CardConfig` from this head's own config. One place, so the width and the fold
/// cannot drift between the live pane and the transcript (the palette is applied where the
/// lines become rows).
pub(crate) fn card_cfg(cfg: &RenderConfig, fold: Fold) -> card::CardConfig {
    card::CardConfig {
        width: cfg.width,
        mode: match fold {
            Fold::Open => card::DisplayMode::Expanded,
            Fold::Folded => card::DisplayMode::Truncated,
        },
        budget: card::Budget::GENERIC,
        show_id: false,
    }
}

/// The last non-empty line of a growing document, trimmed to fit.
pub(crate) fn last_line(raw: &str, cfg: &RenderConfig) -> String {
    let l = raw
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    trim_to(l.trim(), cfg.width.saturating_sub(4))
}

/// The columns everything the model *does* is set in, under everything anybody
/// *says*.
///
/// # A turn had no shape
///
/// The operator's words: *"user message, then a flat wall of cards. Nothing says
/// this is one assistant turn, nothing separates thinking from acting from
/// answering, and assistant prose has no home of its own."* Every row started in
/// the same column, so a question, a file listing and the answer were three
/// things of equal weight in a stack.
///
/// What separates them here is a **step**, not a new glyph. The operator's
/// question and the model's answer sit at the body's own column — they are the
/// conversation. Thinking and acting are indented one step under them: they are
/// how the answer was arrived at, and they are subordinate to it. The turn's
/// footer rule closes the block at the outer column again.
///
/// That gives a turn four readable levels out of the vocabulary already on the
/// screen — `▌` for the question, a step in for the working, the answer flush
/// left, `──` to close — and costs no colour, so it survives [`Palette::None`]
/// and a copy-paste, which is the same argument the reasoning rail makes.
///
/// **Two columns, matching the reasoning rail's width** (`card::REASONING_RAIL_WIDTH`)
/// and the frame's own gutter, so the page reads as one repeated step rather than
/// as three unrelated indents. Given up below sixty columns, where two columns
/// out of every line is a bigger fraction than the hierarchy is worth — the same
/// trade `App::gutter` makes at forty.
pub(crate) fn activity_indent(w: usize) -> usize {
    if w >= 60 {
        card::REASONING_RAIL_WIDTH
    } else {
        0
    }
}

/// Set `lines` one step in. Empty rows stay empty: trailing spaces on a blank
/// line are invisible until something copies them.
pub(crate) fn step_in(lines: Vec<String>, n: usize) -> Vec<String> {
    if n == 0 {
        return lines;
    }
    let pad = " ".repeat(n);
    lines
        .into_iter()
        .map(|l| if l.is_empty() { l } else { format!("{pad}{l}") })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowClass {
    /// Somebody said something: the operator's question, the model's answer.
    Speech,
    /// The model working: reasoning, and tool calls.
    Activity,
    /// Anything else — a system row, a segment mark, an announcement with no
    /// body yet.
    Other,
}

/// Everything one transcript row needs to know about where it sits.
///
/// A struct rather than seven positional parameters because two of the seven are
/// round-scoped and one is row-scoped, and a caller passing them in the wrong
/// order is exactly the defect this file has just finished fixing.
/// Rows an open payload window leaves for everything else on the screen: the header, the
/// row's own heading, the composer and its rows. Generous rather than exact — a window a
/// line short of the screen is read whole; one a line too tall has lost its first line.
pub(crate) const WINDOW_CHROME: usize = 10;

pub(crate) struct ItemCtx<'a> {
    pub(crate) cfg: &'a RenderConfig,
    pub(crate) think: Fold,
    pub(crate) tools: Fold,
    pub(crate) raw: bool,
    /// Display targets for **this row's round**, keyed by call id.
    pub(crate) targets: &'a std::collections::HashMap<String, String>,
    /// Call ids in this round that already have a settled result row below.
    /// Their card is that row; the assistant row does not draw them again.
    pub(crate) answered: &'a std::collections::HashSet<String>,
    /// **The children this head has watched**, for the one fact a completion notice cannot say
    /// for itself: what a child was asked. Looked up by handle — see [`subagent_asked`], which
    /// the subagents pane and the notice share so the two cannot describe one child differently.
    pub(crate) subagents: &'a [SubagentState],
    /// This row belongs to the turn the live pane is still drawing, so the pane
    /// below owns whatever has not settled and this row draws none of it.
    pub(crate) drawn_live: bool,
    /// How long this row's call took, when this head watched it run.
    pub(crate) elapsed_ms: Option<u64>,
    /// Both sides of the file this row's call changed, when this head watched
    /// it run. See `App::call_edits`.
    pub(crate) edit: Option<&'a letibot_sessionlog::event::ToolEdit>,
    /// The settled decision this row's call was gated by, when there was one.
    /// See `App::call_decisions`.
    pub(crate) decision: Option<&'a letibot_sessionlog::view::SettledDecision>,
    /// The echo this head bound to this row, when the row is a `user` row whose body
    /// has not arrived. See `App::bound_prompts`: it is drawn **in the row's place**,
    /// which is what puts the prompt above the reply it caused instead of below it
    /// and tagged `queued`.
    pub(crate) bound: Option<&'a str>,
    /// **Which mark a bound echo carries** — `queued` or `unconfirmed` (R16).
    ///
    /// On the context rather than derived from `bound`, because whether a snapshot could
    /// resolve this echo is a fact about the *head's* history and not about the text: the
    /// same words are `queued` when a row is expected and `unconfirmed` when a snapshot
    /// has already replaced the transcript without carrying it. Only `App` knows which.
    pub(crate) echo_mark: &'a str,
    /// **Whether an echo is drawn in full or as its elided headline** (R33).
    pub(crate) echo_open: bool,
    /// **Which set of switches this row is being drawn for** (R37).
    ///
    /// On the context rather than read from the app, because `item_lines` is a free function
    /// and the walk holds the app apart — the same reason every other field here is passed.
    ///
    /// **A set and not a rung**, and that is this slice's one new drawing: `keeps` is the
    /// ladder's answer with the edit card's exception, and the renderer and the run finder ask
    /// it here rather than asking the ladder and patching its answer afterwards.
    pub(crate) vis: Visibility,
    /// The operator's diff-view choice (`/config`); the width decides the rest.
    pub(crate) diff_split: bool,
    /// How far into a row's payload the reader has paged, and which row that is.
    ///
    /// A pair because "the view is open" and "how far down it is" have to agree about
    /// *which* row — several payloads can be unfolded on one screen, and a bare offset
    /// would page all of them together. Keyed on the **item id**, which is what
    /// `item_lines` holds; keying it on the call id is a mismatch that leaves the view
    /// silently closed, and it was: the first version did exactly that and the test
    /// caught it (the seam said `ctrl-t pages` while `ctrl-t` had been pressed).
    pub(crate) payload_view: Option<(&'a str, usize)>,
    /// Where the draw records the open window's furthest full page (see `App::payload_max`).
    pub(crate) payload_max: Option<&'a std::cell::Cell<usize>>,
    /// **The most rows an open payload window may take**: the screen's, less the chrome.
    /// The window's budget is a fixed forty rows, and on a shorter terminal its top was
    /// above the screen — the first twenty lines of a result opened to be read, unreachable
    /// while the window held the keys. `usize::MAX` where there is no screen to fit.
    pub(crate) window_rows: usize,
    /// **The one row `ctrl-t` can act on**, or `None` when no result is long enough
    /// to have a rest to read.
    ///
    /// The seam names a chord, and a chord may only be named where it acts. There is
    /// no cursor in this head, so exactly one row can be addressed — the newest long
    /// result — and it is *this* row; every other row's seam names `/t` instead,
    /// which is the verb that does reach an older row's payload. R10's other half:
    /// the chord used to flip the whole conversation's fold AND seed this one row's
    /// window, so a seam that read per-row announced a wall.
    pub(crate) payload_newest: Option<&'a str>,
}

pub(crate) fn item_lines(it: &SnapshotItem, ctx: &ItemCtx<'_>) -> (RowClass, Vec<String>) {
    let ItemCtx {
        cfg,
        payload_newest,
        bound,
        echo_mark,
        echo_open,
        vis,
        ..
    } = *ctx;
    // **The set, before anything else** (R37). A row this set does not keep renders to
    // nothing, and the walk already treats a row that renders to nothing as no row at all —
    // no separator, no span, no height — so a hidden row costs this function one early
    // return.
    //
    // **And it is the SAME question the run finder asks** ([`row_drawn_at`]): an edit card
    // kept by `read-edits` is a row here and a drawn row there, and the two agreeing is what
    // stops a marker being drawn beside a row that is still on the screen.
    //
    // **R37 AMENDED, and the two are not alternatives.** A row inside an OPEN run arrives here
    // as the lifted set, because opening a run is the rung lifted for its rows and nothing
    // else — so this function does not know about runs at all. A row inside a CLOSED run is
    // never drawn as a row: the walk answers that one line for the whole run, at the run's
    // first row ([`hidden_run_lines`]), and the rows behind it render to nothing here. That is
    // one line per RUN, which is what the amendment asks for and is not a placeholder per row.
    if let Some(item) = it.item.as_ref()
        && !vis.keeps(item)
    {
        return (RowClass::Other, Vec::new());
    }
    // The live pane's own echo of a row it has bound is `User` by construction, so it
    // survives; a row whose body has not arrived carries no item at all and is drawn from
    // the echo, which is also the conversation's.
    let newest = payload_newest == Some(it.item_id.as_str());
    let ind = activity_indent(cfg.width);
    let Some(item) = &it.item else {
        // **A row with no body — unless this head has bound an echo to it.**
        //
        // The announcement carries an id and a kind and no text, and the body follows
        // on its own channel. For a `user` row that is this head's own prompt the head
        // already holds the words, so the row is drawn from them here — in the row's
        // own place, which is above the reply the model is already streaming. See
        // `App::bound_prompts` for why the binding is a guess and why the block keeps
        // the echo's `queued` shape rather than taking the settled one: the
        // announcement cannot say whether this row is this head's prompt at all, and
        // `queued` is exactly the word for "bound, not yet confirmed by content".
        if let Some(text) = bound {
            return (
                RowClass::Speech,
                queued_lines(text, cfg, echo_mark, echo_open),
            );
        }
        // **The announcement arrived and the body has not — so draw nothing.**
        //
        // This used to render `[kind — waiting for the body of s-…]`, one line per
        // row, which was tolerable when the state lasted a frame in the middle of a
        // turn. A fork made it intolerable: `/reseat` carries the whole conversation
        // across and publishes an announcement for every item before a single body
        // follows, so the operator got thousands of them at once — *"i again so
        // insane amount of grainess with s- and whatever tool lines"*.
        //
        // A row with no body is not information, and a screen full of identical
        // placeholders is not a diagnostic — it is noise with the shape of one. What
        // IS worth saying is how far along the carry is, and that is one line at the
        // tail with the cat and the bar the prefill already uses; see
        // `App::filling_line`. Both callers drop a render with no lines, so
        // returning none is how a row says "not yet".
        return (RowClass::Other, Vec::new());
    };
    match item {
        TranscriptItem::System { text, origin } => {
            let mut out = vec![dim(cfg, &format!("system ({origin:?})"))];
            out.extend(
                wrap(&without_control_lines(text), cfg.width)
                    .into_iter()
                    .map(|l| dim(cfg, &l)),
            );
            (RowClass::Other, out)
        }
        TranscriptItem::User { .. } => user_row_lines(it, item, ctx),
        TranscriptItem::Reasoning { .. } => reasoning_row_lines(item, ctx, ind),
        TranscriptItem::Assistant { .. } => assistant_row_lines(it, item, ctx, ind),
        TranscriptItem::ToolResult { .. } => tool_result_row_lines(it, item, ctx, newest, ind),
        TranscriptItem::SegmentMark { label, .. } => {
            (RowClass::Other, vec![dim(cfg, &format!("─── {label} ───"))])
        }
    }
}
