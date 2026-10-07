//! **One row of the conversation, drawn**: `item_lines` turns a transcript item into its
//! lines, under the visibility, folds and budgets the frame was asked for.

use crate::app::*;
use crate::markdown::IncrementalMarkdown;
use crate::render::{BlockCache, RenderConfig, trim_to, visible_width, wrap};
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::{TranscriptItem, UserPart};
use letibot_ui::ansi;
use letibot_ui::style::{Painter, Role};
use letibot_ui::text::{without_control, without_control_lines};
use letibot_ui::{card, diff::DiffConfig, sidediff};
use std::borrow::Cow;

/// A `card::CardConfig` from this head's own config. One place, so the width, the
/// palette and the fold cannot drift between the live pane and the transcript.
pub(crate) fn card_cfg(cfg: &RenderConfig, fold: Fold) -> card::CardConfig {
    card::CardConfig {
        width: cfg.width,
        palette: cfg.palette(),
        mode: match fold {
            Fold::Open => card::DisplayMode::Expanded,
            Fold::Folded => card::DisplayMode::Truncated,
        },
        budget: card::Budget::GENERIC,
        show_id: false,
    }
}

/// A left half and a right half of one row, with the gap between them.
///
/// Falls back to the left half alone when both do not fit, because the left half
/// is the one that says what is happening.
pub(crate) fn split_row(left: &str, right: &str, w: usize) -> String {
    let (lw, rw) = (visible_width(left), visible_width(right));
    if lw + rw + 2 <= w {
        format!("{left}{}{right}", " ".repeat(w - lw - rw))
    } else {
        trim_to(left, w)
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

/// Drop a leading line-number gutter — `     1| ` — from one line of tool output.
///
/// Only ever applied to a **one-line preview inlaid on a header**, never to a
/// body: a body's gutter is how a reader refers to a line, and taking it away
/// there would lose a fact. On a header it is `1|` before the only line there is,
/// which is three columns saying "this is line one of one".
///
/// A prefix match rather than a parse of any tool's format. It matches what
/// `read` emits and nothing that is not shaped exactly like it; a tool whose
/// output happens to begin `12| ` gets three columns back and loses nothing.
pub(crate) fn strip_gutter(l: &str) -> String {
    let t = l.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    match t[digits..].strip_prefix("| ") {
        Some(rest) if digits > 0 => rest.trim_end().to_string(),
        _ => l.trim().to_string(),
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
        think,
        tools,
        raw,
        targets,
        edit,
        decision,
        diff_split,
        answered,
        drawn_live,
        elapsed_ms,
        payload_view,
        payload_max,
        window_rows,
        payload_newest,
        bound,
        echo_mark,
        echo_open,
        vis,
        subagents,
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
        TranscriptItem::User { parts, speaker } => {
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
                letibot_transcript::Speaker::Operator => {
                    (RowClass::Speech, user_block(&text, it.ts, cfg))
                }
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
        TranscriptItem::Reasoning { text, .. } => {
            // **The model's reasoning is text this head did not author** (§3.1), and it
            // reaches the terminal through the markdown renderer with the head's own
            // escapes around it — so a control byte in it is a control byte on the
            // glass. Sanitised BEFORE the lexer, so what is lexed and what is measured
            // are the same string: the escape becomes one space, which is what keeps the
            // column arithmetic honest (`without_control`'s whole argument).
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
        TranscriptItem::Assistant {
            text, tool_calls, ..
        } => {
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
                let box_cols = crate::backend::graphics::image_box(cfg.width);
                let mut from = 0;
                for (alt, target) in crate::render::markdown_images(text) {
                    let Some((id, pw, ph)) = crate::render::reply_image(&it.item_id, &target)
                    else {
                        continue;
                    };
                    let (cols, rows) = crate::backend::graphics::image_cells(pw, ph, box_cols);
                    let picture = crate::backend::graphics::image_rows(id, cols, rows);
                    let at = crate::render::picture_anchor(&out, from, &alt, &target)
                        .unwrap_or(out.len());
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
        TranscriptItem::ToolResult {
            name,
            outcome,
            payload,
            call_id,
            edit: row_edit,
            origin,
            media,
        } => {
            // **The picture itself, where the terminal can draw one** (kitty graphics, Unicode
            // placeholders): rows of text the terminal fills with the image the head uploaded
            // under this row's id. PNG only — the protocol takes it as it is — and every other
            // format keeps the line the payload already says. Built here and appended at each
            // way out of this arm, the one-line form's included.
            let picture: Vec<String> = match media {
                Some(m) if cfg.images && m.mime == "image/png" => {
                    let (cols, rows) = crate::backend::graphics::image_cells(
                        m.width,
                        m.height,
                        crate::backend::graphics::image_box(cfg.width),
                    );
                    crate::backend::graphics::image_rows(
                        crate::backend::graphics::image_id(&it.item_id),
                        cols,
                        rows,
                    )
                    .into_iter()
                    .map(|r| format!("  {r}"))
                    .collect()
                }
                _ => Vec::new(),
            };
            // The row's own excerpt, when it has one — every row the runtime
            // builds now carries the same bounded pair the event does, so a
            // row read out of a store draws its diff without this head having
            // watched the call run. The live copy wins when both exist: it is
            // what this head saw, and the two are built by the same helper
            // (`runtime::bounded_edit`), so disagreement would mean a bug
            // rather than a choice.
            let edit = edit.or(row_edit.as_ref());
            // **The envelope is addressed to the model, not to the operator.**
            //
            // `<<<TOOL_ERROR 5ebfdef6>>>` and its `<<<END_…>>>` are how a result
            // tells the model where the harness's text stops and the payload starts
            // — a marker, with a per-call nonce so a payload cannot forge one. On a
            // screen it is a line of noise in the middle of the two lines a folded
            // row has, and the operator reads a random hex string where the result
            // should be.
            // **And the bytes are the command's, not this terminal's.**
            //
            // A payload is whatever a tool wrote, escape sequences included, and
            // rendering it straight puts those on the wire to the operator's
            // terminal. Measured in their store, 2026-09-20: 44 `tool_result`
            // rows carry an escape and **20 carry a mode string** — `?1002`,
            // `?1006`, `?1049`, `?2004` — which are mouse tracking, the alternate
            // screen and bracketed paste.
            //
            // That is the bug they reported as *"when i expand tools with Ct
            // scroll stops working, even after collapsing back. i have to switch
            // byobu windows back and forth"*, and their own guess at it — *"maybe
            // it is different escapes?"* — was right. Ctrl+T renders payloads that
            // were folded away; one of them turns mouse reporting off; the wheel
            // stops scrolling; folding back does not put the mode back because the
            // terminal has already been told; and switching windows fixes it
            // because tmux re-asserts its modes on focus.
            //
            // Sanitised here rather than in the store: the record is what the tool
            // wrote and must stay that. A space rather than a deletion, because
            // the wrapper about to measure these lines counts columns.
            //
            // **And since 2026-09-25 it is sanitised AND painted** (§3.1's second half).
            // The operator's own run now reaches a terminal (`exec::pty`), so a command
            // that colours — `ls`, `grep --color`, `cargo` — writes SGR into this payload,
            // and the sanitiser this line used to call would **delete the colour it just
            // made possible**: *"i run `! ls -la` and the output is plain, while in a proper
            // terminal directory names are highlighted"* would have been answered on the
            // run half and thrown away here. `letibot_ui::ansi::painted` is the same walk
            // with SGR interpreted into the palette's own roles; everything that is not
            // SGR is still removed, whole, exactly as before.
            //
            // **On the text, and that is what keeps the fold honest.** The envelope
            // filter, the count below and every comparison against `lines` are taken from
            // the sanitised text — one entry per line of payload, no escape byte in sight —
            // so `+N lines` counts lines a reader can read and a painted line's escapes
            // cannot inflate it. `trim_to` and the wrapper measure columns escape-aware, so
            // a painted line is cut where an unpainted one would be.
            let flat: Vec<String> = payload.lines().map(without_control).collect();
            // The raw bytes beside the text of each line that survives the envelope
            // filter: **the painter needs the sequence and the fold needs the text**, and
            // one vector cannot be both.
            let kept: Vec<(&str, &str)> = payload
                .lines()
                .zip(flat.iter())
                .filter(|(_, clean)| !is_envelope(clean))
                .map(|(raw, clean)| (raw, clean.as_str()))
                .collect();
            let lines: Vec<&str> = kept.iter().map(|(_, clean)| *clean).collect();
            let bad = !matches!(outcome, letibot_transcript::ToolOutcome::Ok);
            let mark = if tools.is_open() { "▾" } else { "▸" };
            // `▾ Read crates/ui/src/style.rs · ok · 183 lines · ctrl-v`, not
            // `▾ read(call_0) …`. The verb and the target are the two words a
            // person scans a settled call for, and the id — a correlation key —
            // takes their place only when the target is not known.
            let verb = card::Verb::of(name).label(false).to_string();
            let subject = match targets.get(call_id) {
                Some(t) if !t.is_empty() => t.clone(),
                _ => format!("({call_id})"),
            };
            // How long it took, when this head watched it run. Carried from the
            // live card at the moment the transcript took the call over — a
            // `TranscriptItem::ToolResult` has no timestamps of its own — and
            // simply absent for a row read out of a snapshot, which is the same
            // rule `card::Phase::Replayed` follows and for the same reason.
            let took = match elapsed_ms {
                Some(ms) => format!(" · {}", letibot_ui::progress::duration(ms)),
                None => String::new(),
            };
            // # Everything used to be the same weight
            //
            // A one-line `ls` and a two-hundred-line search rendered identically:
            // one grey header, one dim body. The operator's words — *"size,
            // indentation and rule-weight should tell you what matters before you
            // read a word"*.
            //
            // The header is now built out of roles rather than painted one colour,
            // and the roles are chosen so the **scan** works with no reading at
            // all:
            //
            // - The subject — the path, the pattern — is [`Role::Plain`], i.e. no
            //   sequence at all, so it is the brightest thing on the row. It is
            //   what a person is looking for.
            // - Everything structural around it is [`Role::Faint`]: the glyph, the
            //   verb, the separators, the chord. Present, skippable.
            // - `ok` is faint too. It is the boring case and it is most of them;
            //   anything else keeps its own loud role, which is §8.2's rule
            //   (abstention must not read like success) and is now the *only*
            //   coloured thing on an ordinary row.
            // - The line count is [`Role::Strong`] once the output is big enough
            //   to be worth a fold — that is the size signal, and it is an
            //   attribute rather than a second colour, so it survives a
            //   terminal-native theme.
            //
            // Under [`Palette::None`] the words are unchanged and the count is
            // still a number, which is the whole reason the weighting is carried
            // by *which* field rather than by a decoration.
            const BIG: usize = 40;
            let p = cfg.palette();
            // **The painter for the payload's own block, which is dim.** `dim` below opens
            // the body with `sgr::DIM`, so a coloured run inside it has to close back to dim
            // rather than to the terminal's default — the defect `Painter` exists for, and
            // the reason this is not a `Palette`. Under `Palette::None` it paints nothing and
            // the line is the sanitised text, which is what `--replay` and CI need.
            let painter = Painter::inside(p, Role::Faint);
            let w = cfg.width.saturating_sub(ind).max(20);
            let outcome_role = outcome_role(outcome);
            let size_role = if lines.len() >= BIG {
                Role::Strong
            } else {
                Role::Faint
            };
            // # It degrades by shortening the subject, never by losing the tail
            //
            // The same rule `header_line` had to learn, and for the same reason:
            // this row was built left to right and trimmed at the right, so a long
            // target ate the outcome. Measured on the operator's session — an
            // `ask_code` call that did not run rendered
            // `▸ ask_code "Give an overview of the crate architecture: what each…`
            // with the word `not run` cut off the end, which is a failed call
            // wearing the shape of a successful one.
            //
            // So the tail is measured first and the subject is given what is left.
            // A path is shortened from its LEFT at a separator — the end of a path
            // is what identifies it, and `crates/tui/src/…` names nothing.
            let word = outcome_word(outcome);
            let tail_cols = 3
                + visible_width(word)
                + visible_width(&took)
                + 3
                + 6
                + lines.len().to_string().len();
            // # A call the person ran says so, in the mark this file already keeps for them
            //
            // MEASURED, and this is the whole of the report: the operator typed `! ls` and
            // `! ls -la` on a live head, the two rows landed correctly — their own `User` line
            // and the `bash` result carrying `origin: CallOrigin::Operator` — the row was
            // drawn, folded and visible at every rung since R24 part two's filter clause, and
            // their words about what they saw were *"no colors tho?"*.
            //
            // **The registers below were already the model's, and that is a fact about the
            // paint rather than a promise.** Every role on this header is chosen from
            // `outcome`, `name`, the payload and the fold — nothing in this arm has ever read
            // `origin` — so an operator-origin row and a model-origin one render byte for byte
            // the same header, the same count, the same body and the same fold. What the
            // operator's row did not have is the one thing a model's row has no need to say:
            // WHO acted. A tool row names its call and never its author, which is right for the
            // model's — there is exactly one proposer — and wrong for the person's, and
            // `origin` is the field the row carries precisely so a head can tell them apart
            // ([`operator_act`] reads the same fact for the filter).
            //
            // So the row wears the `▌` bar in [`Role::UserAccent`]: the glyph and the role
            // `user_block` already gives the operator's own words, and the one their `! ls`
            // line is wearing two rows above. **The same glyph and the same role rather than a
            // new colour**, because the palette has exactly one meaning for *the person at the
            // keyboard* and inventing a second would make two spellings of one fact. And it is
            // a GLYPH as well as a colour, which is the whole reason it is this mark: under
            // [`Palette::None`] — the pipe, `--replay` and CI case — a provenance carried by a
            // colour alone would say nothing at all, and the bar survives with no sequences and
            // survives a copy-paste, the argument `user_block`'s own note already makes.
            //
            // **What it does not change.** The fold, the count, the one-line form, the diff,
            // the reason, the decision block and every other row on the screen: this is two
            // columns of the header, and it is measured into `lead` below so a long subject is
            // shortened by the same arithmetic that shortens it for a model. **And it invents
            // no target.** For the operator's `!` line the daemon mints the id itself (`bang-N`)
            // and runs the line, and no event carries the arguments back — the head is told the
            // line was queued and nothing else — so `targets` has nothing to offer and the row
            // still names the call id where a model's names the command. That is the honest
            // degradation: the command is the operator's own `User` line, verbatim, one row
            // above, which is a place a model's call has nothing in.
            let mine = matches!(
                origin,
                Some(letibot_transcript::CallOrigin::Operator { .. })
            );
            let provenance = if mine {
                format!("{} ", p.paint(Role::UserAccent, "▌"))
            } else {
                String::new()
            };
            let lead = format!("{provenance}{mark} {verb} ");
            let subject = shorten_subject(
                &subject,
                w.saturating_sub(visible_width(&lead) + tail_cols).max(8),
            );
            let mut head = provenance;
            head.push_str(&p.paint(outcome_role, mark));
            head.push_str(&p.paint(Role::Faint, &format!(" {verb} ")));
            // **The file, as a link** (OSC 8) where the terminal speaks it: the full target
            // the shortened subject stands for, so a click opens the file and not `…/app.rs`.
            let painted = p.paint(Role::Plain, &subject);
            let painted = match (&cfg.links, targets.get(call_id), card::Verb::of(name)) {
                (
                    Some(root),
                    Some(full),
                    card::Verb::Read | card::Verb::Edit | card::Verb::Write | card::Verb::List,
                ) => crate::backend::links::file_link(root, full, &painted),
                _ => painted,
            };
            head.push_str(&painted);
            head.push_str(&p.paint(outcome_role, &format!(" · {word}")));
            head.push_str(&p.paint(Role::Faint, &took));

            // A result of one line goes ON the header. `▸ Read .gitignore · ok ·
            // 1.1s · /target` is one row where `▸ Read .gitignore · ok · 1 line ·
            // ctrl-t` over `  /target` was two, and the second of them carried the
            // count and the chord for a fold that has nothing to fold. At 34 rows
            // that halving is the difference between four calls fitting and eight.
            //
            // **And it is the one place on this row that does not paint.** `lines` is the
            // *sanitised* text — `without_control`'s — so a one-line payload's SGR is removed
            // whole and its colour goes with it: `! ls` is drawn plain while `! ls -la`'s four
            // lines are drawn coloured by the body below. That is a **loss** and not a
            // passthrough, and it is the only disagreement between the two readers on the
            // operator's own row — `letibot_ui::ansi`'s header states the same fact from the
            // other side. Left as it is rather than painted here: the header's registers are
            // `Faint`/`Plain` and the one-line form's whole argument is that the payload's text
            // is part of the header, so colouring it is a decision about the header and not a
            // fix to the payload. A one-line `! ls` is the case to weigh if that changes.
            let inline = (!bad
                && lines.len() == 1
                // An edit with an excerpt draws its diff, not the tool's prose —
                // the rule the folded arm below already follows. The one-line
                // shortcut used to preempt it: a landed edit whose payload was a
                // single line put the prose on the header and returned before the
                // diff block, so the change was nowhere on the screen even when
                // the head held both sides.
                && edit.is_none())
            .then(|| strip_gutter(lines[0]))
            .filter(|l| !l.is_empty())
            .filter(|l| visible_width(&head) + 3 + visible_width(l) <= w);
            if let Some(l) = inline {
                head.push_str(&p.paint(Role::Faint, " · "));
                head.push_str(&p.paint(Role::Plain, &l));
                let mut out = vec![trim_to(&head, w)];
                // The approval rides the one-line form too: a gated call whose
                // result fit on the header is no less gated for it.
                if let Some(d) = decision {
                    out.extend(decision_lines(d, tools, w, p));
                }
                out.extend(picture);
                return (RowClass::Activity, step_in(out, ind));
            }

            head.push_str(&p.paint(
                size_role,
                &format!(
                    " · {} line{}",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                ),
            ));
            // No `· ctrl-v` here. The chord belongs on the elision row below, which
            // exists exactly when something is hidden — an affordance on a card
            // with nothing folded is eight columns of every row spent advertising
            // a key that would do nothing, and the hint bar already teaches it.
            let mut out = vec![trim_to(&head, w)];
            // The reason, on its own wrapping line rather than in the header's
            // tail. Never folded, never truncated, and in the outcome's own role:
            // a call that abstained or was refused said *why*, and that sentence
            // is the whole content of the row.
            // **Owned, and it has to be**: `w` is this closure's own temporary, so a borrowed
            // `Cow` would be a reference to a value that dies at the end of the closure. This is
            // the one place in the head where the sanitiser's fast path cannot be taken, and the
            // compiler is what found it.
            let why = outcome_why(outcome).map(|w| without_control_lines(&w).into_owned());
            // **A reason that is a DOCUMENT is not a sentence.**
            //
            // This printed the reason in full, unfoldable, on the argument that a
            // refusal nobody can read is a refusal nobody acts on. That holds while
            // the reason is a sentence. Layer A's is not: it names every construct
            // it could not resolve, one indented paragraph each, and ends with the
            // instruction to re-issue — twenty lines of prose addressed to the
            // MODEL, which the head then painted into the operator's chat. Measured
            // on a six-line shell loop; the operator's answer was *"i get what it
            // tries to do, but it just throws up on my chat"*.
            //
            // So the first sentence stands unfolded — what happened, always visible,
            // which is what the original rule was protecting — and the rest arrives
            // with ctrl-t like every other long thing on this screen.
            let mut why_folded = false;
            if let Some(why) = &why {
                // **The payload says it too, so this stays a gist.** Unfolding is
                // what reveals the payload, and a refusal's payload is a complete
                // explanation — it names the decider, the basis and what to do.
                // Printing the whole `why` above it meant ctrl-t produced the same
                // paragraph twice in one card, three times counting the envelope's
                // own `outcome:` line. The operator, counting: *"how many times is
                // 'nothing ran' needed?"*
                //
                // Once. When the reason is NOT below, unfolding still shows all of
                // it, because then this is the only place it is said.
                // Matched on the reason's FIRST LINE: `why` is a paragraph and
                // `lines` is the payload already split, so a whole-paragraph
                // containment can never hit. One line of forty-plus characters
                // appearing verbatim below is not a coincidence.
                let first = why.lines().next().unwrap_or("").trim();
                let echoed = first.len() >= 40 && lines.iter().any(|l| l.contains(first));
                let shown: Cow<'_, str> = if tools.is_open() && !echoed {
                    Cow::Borrowed(why)
                } else {
                    let gist = first_sentence(why);
                    why_folded = gist.len() < why.len();
                    gist
                };
                out.extend(
                    wrap(&shown, w.saturating_sub(2))
                        .into_iter()
                        .map(|l| p.paint(outcome_role, &format!("  {l}"))),
                );
            }
            // The decision this call was gated by, in the dim register — the same
            // block the live card draws, carried across with the card. Without this
            // the approval leaves the screen the moment the result row takes the
            // call over.
            if let Some(d) = decision {
                out.extend(decision_lines(d, tools, w, p));
            }
            // Folded shows the first line, which is where a tool puts what it did.
            //
            // A failure used to be exempt — *an error nobody can read is an error
            // nobody acts on* — and that rule is satisfied by the line above,
            // which prints the reason in full, wrapped, unfoldable. What the
            // exemption was actually doing on the screen was printing a tool's
            // whole `<<<TOOL_ERROR>>>` envelope, in which the reason appears twice
            // more. So the exemption now applies only when there is **no** reason
            // to have printed: a timeout, where the payload is all there is.
            // **A file edit draws its diff, not the tool's prose.** The tool's
            // payload is addressed to the model — "path: 1 replacement(s)" and a
            // window of the new file — and a folded row showed two lines of it.
            // When this head watched the call run it holds both sides, and the
            // operator's question about an edit is "what changed", which is a
            // diff in whichever of the two shapes the toggle picks
            // (`sidediff::edit_view`).
            // Folded keeps the first hunk's opening rows so the change is on the
            // screen without the fold; open shows it whole, up to the diff's own
            // cap. A row this head did not watch run has no pair and keeps the
            // prose, which is the `Replayed` rule.
            if let Some(e) = edit
                && matches!(card::Verb::of(name), card::Verb::Edit | card::Verb::Write)
                && !bad
            {
                let dcfg = DiffConfig {
                    width: w.saturating_sub(2),
                    palette: p,
                    context: 3,
                    line_numbers: true,
                    intra_line: false,
                    max_rows: 60,
                };
                let view = sidediff::edit_view(diff_split);
                // **The same rule as the live card's** (§3.1): a diff is a file's
                // bytes and this head did not author them. Sanitised before the
                // diff is taken so the two sides compared are the two sides
                // shown.
                let path = without_control_lines(&e.path);
                let before = without_control_lines(&e.before);
                let after = without_control_lines(&e.after);
                let mut rows = sidediff::render_edit_view(
                    &path,
                    &before,
                    &after,
                    e.before_start,
                    e.after_start,
                    &dcfg,
                    view,
                );
                if header_names_the_file(&subject, &path) && !rows.is_empty() {
                    rows.remove(0);
                }
                if e.truncated {
                    rows.push(p.paint(
                        Role::Faint,
                        &format!(
                            "… the excerpt was capped; the file is {} lines now",
                            e.after_lines
                        ),
                    ));
                }
                let keep = if tools.is_open() {
                    rows.len()
                } else {
                    8.min(rows.len())
                };
                let hidden = rows.len() - keep;
                out.extend(rows.into_iter().take(keep).map(|l| format!("  {l}")));
                if hidden > 0 {
                    out.push(p.paint(
                        Role::Faint,
                        &format!("  … +{hidden} diff rows · /t unfolds it"),
                    ));
                }
                return (RowClass::Activity, step_in(out, ind));
            }
            // **The payload's own window, which is what makes the rest of it
            // reachable.**
            //
            // Under the fold this card may draw two rows; opened, the body budget. Either
            // way it was drawn from the **head** — so for a 418 KB log the fold reported
            // `… +N lines` and the chord revealed nothing, because opening the fold
            // changed the *budget*, not the *offset*. There was no offset.
            //
            // So a row whose view is open draws a window into its payload and the arrows
            // page it. `payload_view` carries *which* row, because several payloads can
            // be unfolded on one screen and a bare offset would page all of them.
            let window = payload_view.is_some_and(|(id, _)| id == it.item_id.as_str());
            let total = lines.len();
            // **The window is the row's own length, not the fold's.** This read
            // `window && tools.is_open()`, so the only way to give one result its rest was
            // to unfold every result in the conversation — which is what made `ctrl-t` a
            // wall. See [`ItemCtx::payload_newest`].
            let shown_rows = if window {
                cfg.budget.body_lines.min(window_rows).max(4)
            } else if tools.is_open() || (bad && why.is_none()) {
                cfg.budget.body_lines
            } else {
                2
            };
            // **The last page is a full one.** It clamped to `total - 1`, so the end of a
            // long output was one line under a seam; the furthest useful offset is the one
            // whose window ends on the last line (a window with the `↑` seam above it).
            let max_page = total.saturating_sub(shown_rows.saturating_sub(2).max(1));
            if window && let Some(cell) = payload_max {
                cell.set(max_page);
            }
            let page = match payload_view {
                Some((_, p)) if window => p.min(max_page),
                _ => 0,
            };
            // One row is spent on the seam when there is more payload, on either side.
            let above = page > 0;
            let body = shown_rows
                .saturating_sub(1)
                .saturating_sub(usize::from(above))
                .max(1);
            let end = (page + body).min(total);
            let below = end < total;
            if above {
                out.push(p.paint(
                    Role::Faint,
                    &format!("  ↑ {page} more lines above · ↑ scrolls up"),
                ));
            }
            out.extend(
                kept[page..end]
                    .iter()
                    .map(|(raw, _)| dim(cfg, &format!("  {}", ansi::painted(painter, raw)))),
            );
            if below {
                let hidden = total - end;
                // grok-build's `execute.rs:549` form, kept: the seam where content was
                // taken out, not a sentence. **And it says which key now does what** —
                // the chord opens the view, the arrows move inside it, and a row that
                // named only the chord was the row that could not be read past its head.
                out.push(p.paint(
                    Role::Faint,
                    &if window {
                        format!("  … +{hidden} lines · ↓ pages down · esc closes")
                    } else if newest {
                        // **The chord, on the row it acts on.** `ctrl-t` opens the window
                        // into the newest long result, and this is that row.
                        format!("  … +{hidden} lines · ctrl-v opens it")
                    } else {
                        // **Not this chord.** `ctrl-t` acts on the newest long result and
                        // there is no cursor in this head to point it at an older one, so
                        // this row names the verb that does reach it: `/t` unfolds every
                        // tool row, and the newest one's window can then be paged. A seam
                        // that named `ctrl-t` here is what the operator met as a wall.
                        format!("  … +{hidden} lines · /t unfolds it")
                    },
                ));
            } else if window {
                // The end of the payload: say so, so "no more" is not confused with
                // "the arrow stopped working".
                out.push(p.paint(Role::Faint, "  … end of output · esc closes"));
            } else {
                // The payload was short enough to show whole, but the REASON was
                // cut — so the affordance has to be here, or the rest of it would
                // be hidden behind a chord nothing on the row mentions.
                if why_folded {
                    out.push(p.paint(Role::Faint, "  … the rest of the reason · /t unfolds it"));
                }
            }
            out.extend(picture);
            (
                RowClass::Activity,
                step_in(out.into_iter().map(|l| trim_to(&l, w)).collect(), ind),
            )
        }
        TranscriptItem::SegmentMark { label, .. } => {
            (RowClass::Other, vec![dim(cfg, &format!("─── {label} ───"))])
        }
    }
}
