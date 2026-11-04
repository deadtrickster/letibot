//! One row of the conversation, drawn: `item_lines` turns a transcript item into its
//! lines, under the visibility, folds and budgets the frame was asked for.

use crate::app::*;
use crate::ui::render::{RenderConfig, trim_to};
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
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

/// The columns everything the model does is set in, under everything anybody says. The question
/// and the answer sit at the body's column; thinking and acting are indented one step under them,
/// as how the answer was arrived at. Two columns match the reasoning rail; none below 60 wide.
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
    /// Anything else - a system row, a segment mark, an announcement with no
    /// body yet.
    Other,
}

/// Rows an open payload window leaves for everything else on the screen: the header, the row's own
/// heading, the composer and its rows. Generous rather than exact - a window a line short of the
/// screen is read whole; one a line too tall has lost its first line.
pub(crate) const WINDOW_CHROME: usize = 10;

pub(crate) struct ItemCtx<'a> {
    pub(crate) cfg: &'a RenderConfig,
    pub(crate) think: Fold,
    pub(crate) tools: Fold,
    pub(crate) raw: bool,
    /// Display targets for this row's round, keyed by call id.
    pub(crate) targets: &'a std::collections::HashMap<String, String>,
    /// Call ids in this round that already have a settled result row below.
    /// Their card is that row; the assistant row does not draw them again.
    pub(crate) answered: &'a std::collections::HashSet<String>,
    /// The children this head has watched, for the one fact a completion notice cannot say
    /// for itself: what a child was asked. Looked up by handle - see [`subagent_asked`], which
    /// the subagents pane and the notice share so the two cannot describe one child differently.
    pub(crate) subagents: &'a [SubagentState],
    /// What this head holds of each ended job's output, by handle - the second line of a
    /// completion notice's compact form (see `job_card`). Asked for by the window that draws the
    /// notice, so a row drawn before the answer lands says what the notice itself knows.
    pub(crate) job_tails:
        &'a std::collections::HashMap<String, crate::ui::transcript::job_card::JobTail>,
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
    /// The session's workspace, which a citation's path is relative to - read to tell whether
    /// the cited lines still say what they said. Empty where there is no file to compare with.
    pub(crate) workspace: &'a str,
    /// The echo this head bound to this row, when the row is a `user` row whose body has not
    /// arrived. Drawn in the row's place, which is what puts the prompt above the reply it caused
    /// instead of below it, tagged `queued`. See `App::bound_prompts`.
    pub(crate) bound: Option<&'a str>,
    /// Which mark a bound echo carries - `queued` or `unconfirmed`. On the context and not derived
    /// from `bound`: the same words are `queued` when a row is expected and `unconfirmed` when a
    /// snapshot already replaced the transcript without it, and only `App` knows which.
    pub(crate) echo_mark: &'a str,
    /// Whether an echo is drawn in full or as its elided headline.
    pub(crate) echo_open: bool,
    /// Which set of switches this row is drawn for. On the context because `item_lines` is a free
    /// function and the walk holds the app apart. A SET and not a rung: `keeps` is the ladder's
    /// answer with the edit card's exception, so the renderer and the run finder cannot disagree.
    pub(crate) vis: Visibility,
    /// The operator's diff-view choice (`/config`); the width decides the rest.
    pub(crate) diff_split: bool,
    /// How far into a row's payload the reader has paged, and which row that is. A pair because
    /// several payloads can be open on one screen and a bare offset would page them all together;
    /// keyed on the ITEM id, which is what `item_lines` holds - the call id leaves it closed.
    pub(crate) payload_view: Option<(&'a str, usize)>,
    /// Where the draw records the open window's furthest full page (see `App::payload_max`).
    pub(crate) payload_max: Option<&'a std::cell::Cell<usize>>,
    /// The most rows an open payload window may take: the screen's, less the chrome. A fixed
    /// forty-row budget put the top of a long result above a shorter terminal, unreachable while
    /// the window held the keys. `usize::MAX` where there is no screen to fit.
    pub(crate) window_rows: usize,
    /// The one row `ctrl-t` can act on, or `None` when no result is long enough to have a rest.
    /// There is no cursor in this head, so exactly one row can be addressed - the newest long
    /// result - and every other row's seam names `/t`, the verb that does reach an older payload.
    pub(crate) payload_newest: Option<&'a str>,
    /// The row drawn just before this one, when it is the session's question to a person (`asked:`,
    /// `asked_card`) - what an operator's row right under it answers. `None` everywhere else.
    pub(crate) replied_to: Option<&'a str>,
}

/// The question the row at `k` answers, when the row before it is the session's question
/// to a person (`asked_card`): the text the walk hands [`ItemCtx::replied_to`].
pub(crate) fn question_before(items: &[SnapshotItem], k: usize) -> Option<String> {
    let prev = items.get(k.checked_sub(1)?)?;
    agent_row_text(prev).filter(|t| crate::ui::transcript::asked_card::question_of(t).is_some())
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
    // The set, before anything else: a row this set does not keep renders to nothing, and the walk
    // treats that as no row at all, so it costs one early return. It is the SAME question the run
    // finder asks, and an OPEN run arrives here already lifted - this function knows no runs.
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
        // A row with no body - unless this head has bound an echo to it. The announcement carries an
        // id and a kind and no text, and the head already holds the words of its own prompt: drawn
        // in the row's own place, above the reply, keeping the echo's `queued` shape.
        if let Some(text) = bound {
            return (
                RowClass::Speech,
                queued_lines(text, cfg, echo_mark, echo_open),
            );
        }
        // The announcement arrived and the body has not, so draw nothing: a placeholder per row was
        // tolerable for a frame, and a fork publishes every announcement before any body follows.
        // What is worth saying is how far the carry is, and that is one line of the status row.
        return (RowClass::Other, Vec::new());
    };
    match item {
        TranscriptItem::System { text, origin } => {
            (RowClass::Other, system_row_lines(text, *origin, it.ts, cfg))
        }
        // A delivery is drawn exactly where the session's own voice has always been drawn - one
        // line a notice, never the operator's block - because the ROW is the harness's, not theirs.
        TranscriptItem::Notice { text, .. } => (
            RowClass::Other,
            crate::ui::transcript::user::session_row_lines(
                text,
                it.ts,
                cfg,
                ctx.subagents,
                ctx.job_tails,
            ),
        ),
        TranscriptItem::User { .. } => user_row_lines(it, item, ctx),
        TranscriptItem::Reasoning { .. } => reasoning_row_lines(item, ctx, ind),
        TranscriptItem::Assistant { .. } => assistant_row_lines(it, item, ctx, ind),
        TranscriptItem::ToolResult { .. } => tool_result_row_lines(it, item, ctx, newest, ind),
        TranscriptItem::SegmentMark { label, .. } => (
            RowClass::Other,
            vec![crate::ui::render::row(
                &rano::agent::screens::segment_mark(label),
                cfg.palette(),
            )],
        ),
    }
}

/// A row the system wrote into the conversation, drawn as ONE line: `system`, what it says, and
/// `click for details` with the text whole in the popup - drawn whole it ran to 190 screen rows.
/// On its own so `walls` measures exactly what the transcript draws.
pub(crate) fn system_row_lines(
    text: &str,
    origin: letibot_transcript::SystemOrigin,
    ts: u64,
    cfg: &RenderConfig,
) -> Vec<String> {
    use crate::ui::transcript::notice_card::system_notice;
    let bootstrap = origin == letibot_transcript::SystemOrigin::Bootstrap;
    let line = system_notice(text, bootstrap).cut(session_text_cols(ts, cfg));
    session_card_block(&[line], ts, cfg)
}
