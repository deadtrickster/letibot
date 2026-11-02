//! **The person's own rows**: their prompts, the session's banner, a queued line not yet sent.

use crate::app::*;
use crate::ui::render::{RenderConfig, row_strings};
use rano::agent::blocks::{QueuedBlock, SessionBlock, UserBlock};

/// A row this SESSION appended, drawn as the session's: a settlement, a salvage notice, a steering
/// line and the intent check all arrive as `User` items, and drawing those as the person typing is
/// the lie the gate refuses. No accent bar, no raised block, the faint register - but the label stays.
pub(crate) fn session_block(text: &str, ts: u64, cfg: &RenderConfig) -> Vec<String> {
    let folded = fold_cells(text);
    let block = SessionBlock {
        text: folded.unwrap_or_else(|| text.to_string()),
        stamp: clock_time(ts),
    };
    row_strings(&block.lines(cfg.width), cfg.palette())
}

/// A session row that opens with a card's line, drawn as [`session_block`] draws one - label, faint
/// text, clock - with each card line's accent in its role: a red target's head in the failure
/// register, a review's verdict in the queue pane's. Already cut to [`session_text_cols`].
pub(crate) fn session_card_block(
    lines: &[crate::ui::transcript::line_cards::CardLine],
    ts: u64,
    cfg: &RenderConfig,
) -> Vec<String> {
    use rano::render::Span;
    use rano::style::Role;
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let block = SessionBlock {
        text,
        stamp: clock_time(ts),
    };
    let mut drawn = block.lines(cfg.width);
    // One drawn line per card line: each was cut to the block's columns, so none wraps.
    if drawn.len() == lines.len() {
        for (l, cl) in drawn.iter_mut().zip(lines) {
            let Some((range, role)) = &cl.accent else {
                continue;
            };
            let Some(i) = l
                .spans
                .iter()
                .position(|s| s.content.starts_with(&cl.text[..range.end]))
            else {
                continue;
            };
            let content = std::mem::take(&mut l.spans[i].content);
            let mut spans = Vec::new();
            if range.start > 0 {
                spans.push(Span::role(content[..range.start].to_string(), Role::Faint));
            }
            spans.push(Span::role(content[range.clone()].to_string(), *role));
            if range.end < content.len() {
                spans.push(Span::role(content[range.end..].to_string(), Role::Faint));
            }
            l.spans.splice(i..=i, spans);
        }
    }
    row_strings(&drawn, cfg.palette())
}

/// The columns a session row's own text has - its label and trailing clock taken off. One
/// function, because the wrap and the trim are two readers of one number: a settlement line trimmed
/// to a width the wrap does not use wraps anyway, which is the defect this exists for.
pub(crate) fn session_text_cols(ts: u64, cfg: &RenderConfig) -> usize {
    // Less the row's own two-column indent, which rano's count leaves out: a line cut to its count
    // drew past the frame by two, and the frame's truncation took the clock - every full-length
    // one-line card lost its `HH:MM:SS` to `HH:MM...`.
    SessionBlock::text_cols(&clock_time(ts), cfg.width)
        .saturating_sub(2)
        .max(8)
}

/// What a child was asked, as one line - the rule the subagents pane and the folded completion
/// notice share. One function because a row and the pane it points at must not disagree; `task` is
/// the subtask and `prompt` the fallback. As words, not markdown: see [`one_plain_line`].
pub(crate) fn subagent_asked(s: &SubagentState) -> String {
    if s.task.is_empty() {
        one_plain_line(&s.prompt)
    } else {
        one_plain_line(&s.task)
    }
}

/// Markdown, as one line of its words: whitespace collapsed, the inline markers gone, through rano's
/// own lexer so a marker reads here as it reads in the transcript. Only the head of the text is
/// lexed ([`PLAIN_LINE_MAX`]), because the pane draws every child on every frame.
pub(crate) fn one_plain_line(text: &str) -> String {
    let collapsed: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(PLAIN_LINE_MAX)
        .collect();
    rano::markdown::lex(&collapsed)
        .iter()
        .map(rano::markdown::Block::title)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// How much of a text [`one_plain_line`] reads: wider than any terminal row.
pub(crate) const PLAIN_LINE_MAX: usize = 512;

/// The daemon's completion notices, folded to one line per settlement: the paragraph is the half
/// addressed to the model. Every fact is kept, and a part this head does not recognise returns
/// `None` and the row is drawn raw, because a fold that swallows text loses information.
pub(crate) fn folded_notice(text: &str, subagents: &[SubagentState]) -> Option<String> {
    let rows = folded_notice_rows(text, subagents)?;
    Some(
        rows.into_iter()
            .map(|(line, _)| line)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The same fold, with the child each line is about: the line, and for a `[task]` settlement's
/// bullet the session id it names - carried out of the same parse that drew the line, so a click
/// and the row cannot disagree. A row this head does not fold is no ids at all.
pub(crate) fn folded_notice_rows(
    text: &str,
    subagents: &[SubagentState],
) -> Option<Vec<(String, Option<String>)>> {
    /// The sentences that prove the rest of a row is advice TO THE MODEL rather than a fact about
    /// the thing reported. One per opening, and named as phrases rather than by position, so a row
    /// whose shape changed stops folding instead of losing the half it did not recognise.
    const PROMISES: &[&str] = &[
        // A completion's closing, `harnessd`'s `JOB_CLOSING` and `TASK_CLOSING` (R93.C1/C2: it
        // used to be a paragraph promising "you do not need to wait for it").
        "Output: `job_output` ",
        "Full answer: `task_result` ",
        // The closing before R93.C1/C2, still in every stored transcript from before it.
        "you do not need to wait for it",
        // The plan's nudge, whose two closings are the operator's row and the model's own. One
        // phrase covers both because it is the phrase both closings contain, and a row left out of
        // `todo_write` stays on the board, so a model's own row is retired by marking it done.
        "mark it done with `todo_write`",
        // **An orphan child's completion** (R89 item 13): told to this session because the one
        // that started it has stopped. `job_card::CLOSINGS` has the same sentence for `[job]`, so
        // the two folds agree about it.
        "This session is told because the one that started it cannot be:",
    ];
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for group in text.split("\n\n") {
        // A held batch opens with one line of its own and the groups under it are the ordinary ones.
        // Folded to what it says before the dash; a heading that runs onto a second line is a shape
        // this head does not know.
        if let Some(said) = group.strip_prefix("[meanwhile] ") {
            if said.contains('\n') {
                return None;
            }
            let said = said
                .split(" — ")
                .next()
                .unwrap_or(said)
                .trim_end_matches(':');
            out.push((format!("Meanwhile: {said}"), None));
            continue;
        }
        let (head, rest) = group.split_once('\n')?;
        // The noun a folded line names itself with, and whether its bullet is a HANDLE or the line
        // itself. A job and a child are collected by an id, so their bullets are backticked handles;
        // a todo nag is collected by nothing, so its bullet is the item's own words and whose row it is.
        let (who, sep): (&str, Option<&str>) = if head.starts_with("[job] ") {
            ("Job ", Some(" "))
        } else if head.starts_with("[task] ") {
            ("Agent ", Some(" · "))
        } else if head.starts_with("[todo check] ") {
            ("Todo ", None)
        } else {
            return None;
        };
        let mut settlements = 0usize;
        let mut promised = false;
        for line in rest.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(bullet) = line.strip_prefix("  - ") {
                // A settlement after the promise is a shape this head does not know, and the
                // one thing it will not do is guess which half of it is the fact.
                if promised {
                    return None;
                }
                settlements += 1;
                let (body, child) = match sep {
                    Some(sep) => {
                        // The handle is everything between the backticks, not the bullet's first
                        // word: a named job's handle carries its label and its id, and splitting on a
                        // space reads `` `release-build `` as the handle and drops the whole row.
                        let (handle, said) = bullet.strip_prefix('`')?.split_once('`')?;
                        let said = said.strip_prefix(' ')?;
                        // A child's own task is LOOKED UP rather than parsed: the notice carries only
                        // the answer, and a second source for what was asked is how the row and the
                        // pane come to disagree. Its handle may carry a label, so the two are split.
                        if who == "Agent " {
                            let (slug, id) = split_child_label(handle);
                            let row = subagents.iter().find(|s| s.session_id == id);
                            let slug: Option<String> = slug
                                .map(str::to_string)
                                .or_else(|| row.map(|s| s.slug.clone()).filter(|s| !s.is_empty()));
                            let name = match slug {
                                Some(slug) => crate::app::child_name(&slug, id),
                                None => id.to_string(),
                            };
                            let said = match row {
                                Some(s) => format!("{} · {said}", subagent_asked(s)),
                                None => said.to_string(),
                            };
                            // **The handle is the session**, whatever the line is called: a named
                            // child is drawn by its name and its short id, and the id is what a
                            // click has to name.
                            (format!("{name}{sep}{said}"), Some(id.to_string()))
                        } else {
                            (format!("{handle}{sep}{said}"), None)
                        }
                    }
                    None => (bullet.trim().to_string(), None),
                };
                out.push((format!("{who}{body}"), child));
            } else if PROMISES.iter().any(|p| line.contains(p)) {
                promised = true;
            } else if promised && super::job_card::is_waiting_line(line) {
                // **The plan rows that wait on this completion**, which the daemon writes after
                // the promise: kept, without its instruction to the model.
                out.push((super::job_card::folded_waiting(line), None));
            } else {
                return None;
            }
        }
        // A heading with nothing under it is not a settlement, and it is not this fold's to
        // summarise: an unknown shape goes to the renderer as it arrived.
        if settlements == 0 {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A child's label split back into its name and handle - `fix-login` then `s-1-sub-3` is
/// `(Some("fix-login"), "s-1-sub-3")`, a bare handle `(None, handle)`. A copy of the tools' own
/// function, since this crate does not link them: only an `s-` handle on the right is a child's.
pub(crate) fn split_child_label(label: &str) -> (Option<&str>, &str) {
    match label.rsplit_once(" · ") {
        Some((slug, handle)) if handle.starts_with("s-") && !slug.trim().is_empty() => {
            (Some(slug), handle)
        }
        _ => (None, label),
    }
}

pub(crate) fn user_block(text: &str, ts: u64, cfg: &RenderConfig) -> Vec<String> {
    // **The renderer sanitises its own input** (§3.1), so a caller cannot forget: rano's
    // block cleans the text before it is wrapped. The operator's own keystrokes cannot carry a
    // control byte - the decoder hands back `Key::Char` - but a *paste* can.
    let folded = fold_cells(text);
    let block = UserBlock {
        text: folded.unwrap_or_else(|| text.to_string()),
        stamp: clock_time(ts),
    };
    row_strings(&block.lines(cfg.width), cfg.palette())
}

/// A `/cells` message with the screen taken back out of it: the transcript keeps the operator's
/// words and one line naming what went with them. Drawing the rows here would be a picture of the
/// terminal inside the terminal, re-wrapped so every box it drew breaks. `None` for no screen.
pub(crate) fn fold_cells(text: &str) -> Option<String> {
    let at = text.find(CELLS_OPEN)?;
    let rest = &text[at..];
    let size = rest
        .strip_prefix(CELLS_OPEN)
        .and_then(|r| r.split_once(' '))
        .map(|(size, _)| size)
        .unwrap_or("");
    // The rows between the two markers; the delimiter lines are not screen.
    let rows = rest
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with(CELLS_CLOSE))
        .count();
    let words = text[..at].trim_end();
    let note = format!("· {rows} rows of this screen ({size}) went with this message");
    Some(if words.is_empty() {
        note
    } else {
        format!("{words}\n{note}")
    })
}

/// A prompt this head has sent that the transcript does not hold yet: the shape a
/// settled user row gets, dimmed, with `queued` where the timestamp goes.
///
/// ```text
///   ▌ queued · also bump the retry budget
/// ```
///
/// The block sits at the tail of the body - the place its row will occupy the
/// moment the step boundary appends it - so a message typed mid-turn never leaves
/// the screen: it changes from `queued` to a timestamped row in place. Dim text
/// rather than the raised block, because the raised block says "this is in the
/// conversation" and until the boundary it is not; the tag is what says what is
/// true instead, in [`Role::Pending`], the colour the spinner already uses for
/// something in flight.
/// **The mark on an echo: `queued` only while nothing is drawing it.**
///
/// R2, and the operator's own order: *"my message | your line | and only then unqueued."* The reply
/// streams (`Delta` carries its text) while the prompt's row is announced in a frame that carries
/// **no** text, its body following later - so a surface that waits for the body to stop saying
/// `queued` is showing the answer to a question it has not drawn yet.
///
/// **`unconfirmed` is the honest word for that window, and it is not a weakening.** The words are on
/// the screen - a row is drawing them - and what the head still owes is the BODY: whether the row's
/// own text will match what was bound. So the three states stay three: `queued` (nothing has it),
/// `unconfirmed` (a row has it and the body is coming), unmarked (it landed).
///
/// **One function because this was two spellings and they disagreed.** The tail asked
/// `claimed_by.is_some() || unconfirmed` while the WALK drew every bound row as `QUEUED` outright,
/// so fixing one left the other drawing the stale mark - and it was the row's own line the operator
/// saw under a streaming reply. MEASURED in
/// `a_prompt_stops_claiming_to_be_queued_when_its_row_is_announced`.
///
/// `drawn` is *is something on this screen drawing these words*: true for the walk, which is
/// drawing the row itself, and `claimed_by.is_some()` for the tail, which is drawing a remainder of
/// it. Free-standing rather than a method because the tail computes it inside the frame's borrow,
/// where only the cloned `unconfirmed` is in hand.
pub(crate) fn echo_mark(unconfirmed: &[String], text: &str, drawn: bool) -> &'static str {
    // The snapshot's doubt outranks everything: the head cannot tell *still coming* from
    // *replaced*, so it keeps saying so.
    if unconfirmed.iter().any(|u| u == text) {
        return UNCONFIRMED;
    }
    // A row is drawing these words, so there is nothing left to claim: `queued` would be false, and
    // `unconfirmed` belongs to the snapshot case above. The row has landed and its body is still
    // coming, which is what a row looks like while it fills in.
    if drawn {
        return "";
    }
    QUEUED
}

/// **The mark on a prompt typed while the daemon is still starting** (R92.2): no worker takes it
/// until the daemon has started, and a bare `queued` read like a daemon that stopped listening.
pub(crate) const QUEUED_UNTIL_STARTED: &str = "queued — runs when the daemon has started";

/// `queued`, and what the daemon is busy with instead, when it has said: `filling` is the `what` of
/// the `Filling` event it is walking, and a prompt nothing has taken while one runs is waiting
/// behind that. The words up to a `:` are its short name, so the mark leaves the prompt room.
pub(crate) fn queued_behind(mark: &str, filling: Option<&str>) -> String {
    match filling {
        Some(what) if mark == QUEUED && !what.is_empty() => {
            let short = what.split_once(':').map_or(what, |(head, _)| head).trim();
            format!("{QUEUED} behind {short}")
        }
        _ => mark.to_string(),
    }
}

pub(crate) fn queued_lines(text: &str, cfg: &RenderConfig, mark: &str, open: bool) -> Vec<String> {
    // Folded here as well as in `user_block`, and it must be the same text going in: the pending row
    // is removed when the transcript's user item matches it, so an abbreviation would leave the
    // `queued` line on the screen for the rest of the session.
    let folded = fold_cells(text);
    let block = QueuedBlock {
        text: folded.unwrap_or_else(|| text.to_string()),
        mark: mark.to_string(),
        open,
    };
    row_strings(&block.lines(cfg.width), cfg.palette())
}

/// `14:32:07` in the local zone, or empty when the row carries no timestamp. Zero is *unknown*, not
/// the epoch: a log recorded before `SnapshotItem::ts` existed replays with zeros, and rendering
/// those as `01:00:00` would be a measurement that was never taken rendered as one that was.
pub(crate) fn clock_time(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let secs = (ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into `tm` and reads `secs`; both are owned here.
    // The `_r` form is the one that does not hand back a shared static, which
    // matters because the driver is not the only thread in this process.
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return String::new();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
