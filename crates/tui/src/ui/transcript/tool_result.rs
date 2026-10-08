//! **A tool's result row**: the header, the diff or the payload's window, the decision that gated
//! it, and the picture it returned — drawn by `rano::agent::tool_row` from the transcript row.
//!
//! What the row's rules are — the header built of roles so the scan works unread, the subject
//! shortened before the tail is lost, the one-line form, the reason's gist, the diff in place
//! of an edit's prose, the payload's window and its seams — is on `rano::agent::tool_row`,
//! with the operator's reports that caused each one. What stays here is this head's half: the
//! transcript's facts mapped onto the widget, and the payload read out of a foreign
//! program's bytes.

use crate::ui::render::row_strings;
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use rano::agent::tool_row::ToolRow;

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
        edit,
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
        edit: row_edit,
        origin,
        media,
    } = item
    else {
        unreachable!("tool_result_row_lines is handed only ToolResult rows");
    };
    // **The picture itself, where the terminal can draw one** (kitty graphics, Unicode
    // placeholders): rows of text the terminal fills with the image the head uploaded
    // under this row's id. PNG only — the protocol takes it as it is — and every other
    // format keeps the line the payload already says. Built here and appended at each
    // way out of this arm, the one-line form's included.
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
    //
    // So each line is read by `letibot_ui::ansi::line` — the sanitiser's own walk with SGR
    // interpreted into the palette's roles, everything that is not SGR removed whole — and
    // the widget filters the envelope, counts and folds by the line's text, which has no
    // escape byte in it.
    let payload = payload.lines().map(letibot_ui::ansi::line).collect();
    let p = cfg.palette();
    let w = cfg.width.saturating_sub(ind).max(20);
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
    let operator = matches!(
        origin,
        Some(letibot_transcript::CallOrigin::Operator { .. })
    );
    let target = match targets.get(call_id) {
        Some(t) if !t.is_empty() => t.clone(),
        _ => String::new(),
    };
    let verb = rano::agent::card::Verb::of(name);
    // **An edit draws its diff, not the tool's prose** — when this head holds both sides; a
    // row it did not watch keeps the prose, which is the `Replayed` rule.
    let diff = edit
        .filter(|_| verb.is_an_edit())
        .map(|e| crate::ui::edit_diff(e, w.saturating_sub(2), cfg, diff_split));
    // **The payload's own window**: `payload_view` carries *which* row, because several
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
        // **The file, as a link** (OSC 8) where the terminal speaks it: the full target the
        // shortened subject stands for, so a click opens the file and not `…/app.rs`.
        link_root: cfg.links.clone(),
        fold: tools.into(),
        window,
        window_rows,
        body_lines: cfg.budget.body_lines,
        newest,
        // **The step is the head's, applied to the strings** — outside the row's own
        // register. A payload row is drawn as its dim body with the step in front of it,
        // and a step taken inside the line would put the step under the dim: the same
        // columns, but not the bytes this row has always been.
        indent: 0,
        ..ToolRow::new(name, call_id, crate::ui::display_outcome(outcome), payload)
    };
    let layout = row.layout(cfg.width.saturating_sub(ind));
    if let (Some(max), Some(cell)) = (layout.max_page, payload_max) {
        cell.set(max);
    }
    (
        RowClass::Activity,
        step_in(row_strings(&layout.lines, p), ind),
    )
}
