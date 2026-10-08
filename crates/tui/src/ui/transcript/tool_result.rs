//! **A tool's result row**: the header, the diff or the payload's window, the decision that gated
//! it, and the picture it returned.

use crate::ui::render::{trim_to, visible_width, wrap};
use crate::ui::*;
use letibot_sessionlog::view::SnapshotItem;
use letibot_transcript::TranscriptItem;
use letibot_ui::ansi;
use letibot_ui::style::{Painter, Role};
use letibot_ui::text::{without_control, without_control_lines};
use letibot_ui::{card, diff::DiffConfig, sidediff};
use std::borrow::Cow;

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
    let picture: Vec<String> = match media {
        Some(m) if cfg.images && m.mime == "image/png" => {
            let (cols, rows) = rano::term::graphics::image_cells(
                m.width,
                m.height,
                rano::term::graphics::image_box(cfg.width),
            );
            rano::term::graphics::image_rows(
                rano::term::graphics::image_id(&it.item_id),
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
    let tail_cols =
        3 + visible_width(word) + visible_width(&took) + 3 + 6 + lines.len().to_string().len();
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
        ) => rano::term::links::file_link(root, full, &painted),
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
