//! A tool's result row: the header, the diff or the payload's window, the decision that gated it,
//! and the picture it returned - drawn by `rano::agent::tool_row` from the transcript row. The row's
//! own rules live there; here is the head's half: the facts mapped on, and a foreign program's bytes.

use crate::ui::render::row_strings;
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use rano::agent::tool_row::ToolRow;

use super::payload_code;

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
        // Read off the row into `App::call_ms` and `App::call_decisions`, which this card is handed.
        ran_ms: _,
        decision: _,
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
    // A write that changed nothing is not `Wrote`: the tool pushes an excerpt for every write it
    // makes, so an `ok` writer with none touched no file, and the reason for that is the row.
    let unchanged = crate::ui::unchanged_write(name, edits, outcome);
    // The envelope is the model's, so it is noise on a screen. What is left is the command's own
    // bytes, drawn as code where the payload has a language and by `letibot_ui::ansi::line` where a
    // program coloured them itself (see [`payload_lines`]).
    let sentences: Vec<&str> = payload.lines().collect();
    let sentences = if unchanged {
        reason_first(&sentences)
    } else {
        sentences
    };
    let payload = payload_lines(
        &sentences,
        !unchanged && cfg.color,
        // The command the call ran, or the file it read: this row draws what that produced, so the
        // call's own target is the one place its language is stated rather than guessed.
        targets.get(call_id).map(String::as_str),
    );
    let p = cfg.palette();
    let w = cfg.width.saturating_sub(ind).max(20);
    // A call the person ran says so: a tool row names its call and never its author, which is right
    // for the model - one proposer - and wrong for the person's, so an operator-origin row wears
    // `user_block`'s own bar and role, a GLYPH because under `Palette::None` colour alone says nothing.
    let operator = matches!(
        origin,
        Some(letibot_transcript::CallOrigin::Operator { .. })
    );
    let shown = if unchanged {
        UNCHANGED.to_string()
    } else {
        ctx.cards.verb(name, edit)
    };
    let target = match targets.get(call_id) {
        // An edit card names its file, and a note's target is its stem and its text. A verb this
        // head chose because nothing was written is not one of those: the call's own target stands.
        _ if shown != *name && !unchanged => edit.map(|e| e.path.clone()).unwrap_or_default(),
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

/// The payload's own sentences, the tool's before the harness's `[note]`s: a folded row shows one
/// line, and for a write that changed nothing the reason is the line worth the room.
fn reason_first<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let (harness, body): (Vec<&str>, Vec<&str>) = lines
        .iter()
        .copied()
        .partition(|l| l.starts_with("[note] ") || l.starts_with("[repaired] "));
    body.into_iter().chain(harness).collect()
}

/// A payload as the rows of its card: a spill seam drawn short, the rest in the roles of the whole
/// payload's one parse — and the text as it came when there is no grid. `named` is the command that
/// made it, which decides the language where the payload's own shape cannot.
fn payload_lines(sentences: &[&str], colour: bool, named: Option<&str>) -> Vec<rano::render::Line> {
    // What a terminal would show of each line, which for a line carrying control bytes is
    // `letibot_ui::ansi::line`'s own reading of it (`sanitize::pieces`: a control byte becomes a
    // space). The grid is built over THESE, so no tab reaches a span whose width would be a guess.
    let shown: Vec<std::borrow::Cow<'_, str>> = sentences.iter().map(|l| drawn_text(l)).collect();
    // A program that coloured its own output is `letibot_ui::ansi::line`'s and is never re-read as
    // code: two registers on one row, and the escape bytes are not the bytes a highlighter was
    // given. One decision for the whole payload, because its rows are one text.
    let coloured = sentences.iter().any(|l| carries_sgr(l));
    // A seam is a marker, not code: its row is [`seam_line`]'s, and the highlighter is handed an
    // empty line where one stands so that a `[...` cannot open a construct which colours the
    // payload under it. Emptied rather than dropped, because row N of the grid is row N here.
    let code: Vec<&str> = sentences
        .iter()
        .zip(&shown)
        .map(|(raw, shown)| {
            if letibot_transcript::seam::Seam::parse(raw).is_some() {
                ""
            } else {
                shown.as_ref()
            }
        })
        .collect();
    let grid = payload_code::grid(&code, colour && !coloured, named);
    sentences
        .iter()
        .zip(&shown)
        .enumerate()
        .map(|(n, (raw, shown))| {
            let roles = grid.as_ref().and_then(|g| g.get(n)).map(Vec::as_slice);
            payload_line(raw, shown.as_ref(), roles)
        })
        .collect()
}

/// A program that coloured its own text: an SGR's `ESC [`, and the C1 `CSI` that says the same.
fn carries_sgr(l: &str) -> bool {
    l.contains('\x1b') || l.contains('\u{9b}')
}

/// The characters a row may draw: the line as it came, or — for a line with a control byte in it —
/// the text `letibot_ui::ansi::line` would have drawn, control bytes read as the spaces they are.
fn drawn_text(l: &str) -> std::borrow::Cow<'_, str> {
    use letibot_transcript::sanitize::Piece;
    if !l.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(l);
    }
    let mut out = String::with_capacity(l.len());
    for piece in letibot_transcript::sanitize::pieces(l) {
        if let Piece::Text(t) = piece {
            out.push_str(&t);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// One payload line as drawn. A spill seam is the model's sentence, too long for a row and read as
/// prose; it is drawn short and in its own role so the cut is visible where it is.
fn payload_line(raw: &str, shown: &str, roles: Option<&[rano::style::Role]>) -> rano::render::Line {
    match letibot_transcript::seam::Seam::parse(raw) {
        Some(seam) => seam_line(&seam),
        None => match roles {
            Some(roles) => payload_code::line(shown, roles),
            None => letibot_ui::ansi::line(raw),
        },
    }
}

fn seam_line(seam: &letibot_transcript::seam::Seam) -> rano::render::Line {
    rano::render::Line::new(vec![rano::render::Span::role(
        format!(
            "··· {} · {} lines not shown · read_spill {} ···",
            crate::ui::render::bytes_human(seam.bytes as u64),
            seam.lines,
            seam.hash
        ),
        rano::style::Role::Info,
    )])
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator's own card: `sed -n` over a few lines of rano's Rust, a keyword, a string and a
    /// comment on it.
    const RUST: &str = r#"    pub fn spawn(&mut self, cmd: &mut Command) -> io::Result<Pty> {
        let mut master = 0;
        // openpty takes a winsize pointer now
        let what = "openpty";
"#;

    fn sentences(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    /// The roles the drawn line carries, top of each span's stack.
    fn roles(line: &rano::render::Line) -> Vec<rano::style::Role> {
        line.spans.iter().map(|s| s.style.top()).collect()
    }

    /// A drawn row as the reader sees it, escapes dropped.
    fn text(row: &str) -> String {
        crate::app::keyhints::cells(row).into_iter().collect()
    }

    #[test]
    fn a_spill_seam_is_drawn_short_and_in_its_own_role() {
        let seam = letibot_transcript::seam::Seam {
            bytes: 152_340,
            lines: 3_011,
            hash: "0123456789abcdef".into(),
            offset: 25_000,
        };
        let drawn = payload_lines(&[&seam.render()], true, None).remove(0);
        assert_eq!(
            drawn.plain(),
            "··· 148.8 KB · 3011 lines not shown · read_spill 0123456789abcdef ···"
        );
        assert!(
            drawn
                .spans
                .iter()
                .all(|s| s.style == rano::render::Style::of(rano::style::Role::Info))
        );
        assert_eq!(payload_lines(&["line 7"], true, None)[0].plain(), "line 7");
    }

    /// The card the operator photographed: `Ran "cd rano && sed -n ..."` over Rust, drawn as code.
    /// Two roles on one line is what says the row is highlighted rather than plain.
    #[test]
    fn a_rust_payload_is_drawn_in_its_roles() {
        let payload = payload_lines(&sentences(RUST), true, None);
        let got = roles(&payload[3]);
        assert!(
            got.contains(&rano::style::Role::Keyword)
                && got.contains(&rano::style::Role::StringLit),
            "a keyword and a string on `let what = \"openpty\";`: {got:?}"
        );
        assert_eq!(payload[3].plain(), "        let what = \"openpty\";");
        assert!(
            roles(&payload[2]).contains(&rano::style::Role::Comment),
            "a comment is a comment: {:?}",
            roles(&payload[2])
        );
    }

    /// And the row is the same text it always was: a payload line is drawn whole, and neither the
    /// grid nor the ANSI walk may move a character of it.
    #[test]
    fn highlighting_moves_no_character_of_the_payload() {
        let plain: Vec<String> = payload_lines(&sentences(RUST), false, None)
            .iter()
            .map(|l| l.plain())
            .collect();
        let lit: Vec<String> = payload_lines(&sentences(RUST), true, None)
            .iter()
            .map(|l| l.plain())
            .collect();
        assert_eq!(plain, lit);
        assert_eq!(plain, sentences(RUST));
    }

    /// A command's output that is prose stays prose — that is every payload before this existed, and
    /// an unknown language is not a reason to paint anything.
    #[test]
    fn a_payload_with_no_language_is_drawn_plain() {
        let out = "   Compiling letibot-tui v0.6.0\n    Finished dev profile in 12.3s\n";
        let drawn = payload_lines(&sentences(out), true, None);
        for l in &drawn {
            assert!(
                l.spans
                    .iter()
                    .all(|s| s.style == rano::render::Style::new()),
                "not one role on {:?}: {:?}",
                l.plain(),
                l.spans
            );
        }
    }

    /// A tab is a control byte, so the row has never drawn one: it draws the space `ansi::line` reads
    /// it as, and the roles come from that same text — a tab in a span would be a width nobody knows.
    #[test]
    fn a_tab_is_drawn_as_the_space_it_has_always_been() {
        let out = "func main() {\n\tx := 1\n}\n";
        let drawn = payload_lines(&sentences(out), true, None);
        assert_eq!(drawn[1].plain(), " x := 1", "the tab, read as a space");
        assert_eq!(
            drawn[1].plain(),
            letibot_ui::ansi::line(sentences(out)[1]).plain(),
            "and the same characters the ANSI walk would have drawn"
        );
        assert!(
            !drawn[1].spans.iter().any(|s| s.content.contains('\t')),
            "no span carries a tab: {:?}",
            drawn[1].spans
        );
    }

    /// A seam is a marker, not code: its own row is the short form, and the row under it keeps the
    /// roles the parse gave it — the `[...` does not open a construct that swallows the payload.
    #[test]
    fn a_seam_is_a_marker_and_the_code_under_it_is_still_code() {
        let seam = letibot_transcript::seam::Seam {
            bytes: 152_340,
            lines: 3_011,
            hash: "0123456789abcdef".into(),
            offset: 25_000,
        };
        let text = format!(
            "    pub fn spawn(&mut self) -> io::Result<()> {{\n{}\n    let what = \"openpty\";\n",
            seam.render()
        );
        let drawn = payload_lines(&sentences(&text), true, None);
        assert_eq!(drawn.len(), 3, "three rows in, three rows out");
        assert_eq!(
            drawn[1].plain(),
            "··· 148.8 KB · 3011 lines not shown · read_spill 0123456789abcdef ···",
            "the seam keeps the head's short form, never its own long sentence"
        );
        assert!(
            mapped(&drawn[1])
                .iter()
                .all(|r| *r == rano::style::Role::Info),
            "and it is one role, not code"
        );
        assert!(
            mapped(&drawn[2]).contains(&rano::style::Role::StringLit),
            "the row under it is still a string: {:?}",
            mapped(&drawn[2])
        );
    }

    /// The roles a drawn line wears, in order.
    fn mapped(line: &rano::render::Line) -> Vec<rano::style::Role> {
        line.spans.iter().map(|s| s.style.top()).collect()
    }

    /// A program that coloured its own output keeps its colours: the whole payload stays on the ANSI
    /// path, so nothing of it is re-read as code and no escape byte is handed to a grammar.
    #[test]
    fn a_payload_with_ansi_is_drawn_exactly_as_the_ansi_walk_draws_it() {
        let out = "\x1b[31mlet mut master = 0;\x1b[0m\n    pub fn spawn(&mut self) {}\n";
        let drawn = payload_lines(&sentences(out), true, None);
        for (n, l) in drawn.iter().enumerate() {
            let want = letibot_ui::ansi::line(sentences(out)[n]);
            assert_eq!(l.plain(), want.plain(), "row {n}, text");
            let (got, expect): (Vec<_>, Vec<_>) = (
                l.spans.iter().map(|s| s.style.clone()).collect(),
                want.spans.iter().map(|s| s.style.clone()).collect(),
            );
            assert_eq!(got, expect, "row {n}, byte for byte");
        }
        assert_eq!(drawn[0].plain(), "let mut master = 0;");
    }

    /// The wiring, through rano's own row: a highlighted payload row reaches the screen painted, and
    /// the header above it does not move.
    #[test]
    fn the_card_draws_the_colours_and_not_only_the_line() {
        let p = rano::style::Palette::terminal(false, false);
        let draw = |lit: bool| -> Vec<String> {
            let row = ToolRow::new(
                "bash",
                "c1",
                rano::agent::outcome::Outcome::Ok,
                payload_lines(&sentences(RUST), lit, None),
            );
            row_strings(&row.layout(120).lines, p)
        };
        let (plain, coloured) = (draw(false), draw(true));
        assert!(
            plain.len() > 1 && coloured.len() == plain.len(),
            "{coloured:?}"
        );
        assert_eq!(
            plain[0], coloured[0],
            "the header is the row's own business"
        );
        assert_eq!(text(&coloured[1]), text(&plain[1]), "the same characters");
        assert_ne!(
            plain[1], coloured[1],
            "the payload row is painted: {:?}",
            coloured[1]
        );
        assert!(
            coloured[1].matches('\x1b').count() > plain[1].matches('\x1b').count(),
            "a keyword, a function name and a type each open their own look: {:?}",
            coloured[1]
        );
    }
}
