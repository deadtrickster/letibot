//! **The person's own rows**: their prompts, the session's banner, a queued line not yet sent.

use crate::app::*;
use crate::render::{RenderConfig, trim_to, visible_width, wrap};
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;

/// **A row this SESSION appended, drawn as the session's** — R42.
///
/// The operator: *"why job completion events arrive as my messages?"* Because every one of
/// them arrives as a `User` item, and this head drew a `User` item the way it draws the person
/// typing — `▌` and the raised block, which is the mark this file reserves for *your own
/// words*. A job settlement, a §5.7 salvage notice, a steering line and the intent check are
/// all *the harness talking*, and drawing them as the operator is exactly the lie the gate
/// already refuses to let them tell the oracle: **the reader is the other party this gate
/// serves.**
///
/// The shape is the opposite of the block, in the three ways the block is made of: no `▌`
/// accent bar, no raised background, and the faint register rather than the operator's own.
/// What it keeps is the label — `session ·`, the same place and shape `queued ·` takes on an
/// echo — because a row nobody can attribute is the defect, not the fix. Wrapped like any
/// prose and sanitised like any other content this head did not author.
pub(crate) fn session_block(text: &str, ts: u64, cfg: &RenderConfig) -> Vec<String> {
    let folded = fold_cells(text);
    let clean = without_control_lines(folded.as_deref().unwrap_or(text));
    let text: &str = &clean;
    let p = cfg.palette();
    let w = cfg.width.max(20);
    let mark = p.paint(Role::Faint, "session · ");
    let stamp = clock_time(ts);
    let mut lines = wrap(text, session_text_cols(ts, cfg));
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut out = Vec::with_capacity(lines.len());
    let indent = " ".repeat(visible_width("session · "));
    for (i, l) in lines.iter().enumerate() {
        let label = if i == 0 { mark.clone() } else { indent.clone() };
        // The timestamp closes the last line rather than the first: this is a note about
        // something that happened, and the block above it puts its stamp on the first row
        // because that row is the person speaking.
        let tail = if i + 1 == lines.len() && !stamp.is_empty() {
            format!("  {stamp}")
        } else {
            String::new()
        };
        out.push(trim_to(
            &format!("  {label}{}", p.paint(Role::Faint, &format!("{l}{tail}"))),
            w,
        ));
    }
    out
}

/// **The columns a `session ·` row's own text has** — its label and its trailing clock taken
/// off.
///
/// One function, because the wrap and the trim are two readers of one number: a settlement line
/// trimmed to a width the wrap does not use is a line that wraps anyway, which is the whole of
/// the defect this exists for. The operator found it by looking: a finished child's row held the
/// entire brief this head had written for that child — *"a giant prompt"* — five wrapped rows of
/// instructions where a settlement should be one line.
pub(crate) fn session_text_cols(ts: u64, cfg: &RenderConfig) -> usize {
    let w = cfg.width.max(20);
    let stamp = clock_time(ts);
    w.saturating_sub(visible_width("session · ") + visible_width(&stamp) + 2)
        .max(8)
}

/// **What a child was asked, as one line** — the rule the subagents pane and the folded
/// completion notice share.
///
/// `task` is the subtask in full and `prompt` is the pre-field fallback: a `done` row drawn from
/// `prompt` showed the child's ANSWER where the operator was looking for what they asked
/// (measured: 122 characters of answer with a two-line task nowhere on the wire). Newlines are
/// collapsed because both callers draw one line — the pane's row, and a notice's settlement line.
///
/// **One function because a row and the pane it points at must not disagree.** The notice looks
/// the task up *by handle*; a second spelling of this rule over there is exactly the drift that
/// would have them say different things about one child.
pub(crate) fn subagent_asked(s: &SubagentState) -> String {
    if s.task.is_empty() {
        s.prompt.clone()
    } else {
        s.task.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// **The daemon's completion notices, folded to one line per settlement.**
///
/// The operator's ask, reading a settled job and a finished child: *"too much … i dont want to
/// see that message to you 'This is the completion…' I also dont care about 'sabagent you
/// started…' it must be something like Job <id> <command summary or wrap> finished <result
/// result summary or wrap> same for agents."* leticl folds these and this head drew the whole
/// paragraph, and the paragraph is the half that is **addressed to the model**: *"you do not
/// need to wait for it, and `job_wait` would only block you…"* is R7's promise being made to the
/// model, which is exactly why a person should not have to read it.
///
/// What is kept is every fact, one per line: the job, how it ended, what it wrote and what it
/// ran — `Job j57 exited 0 after 7m06s, wrote 508 bytes: <command>` — and, for a child, the
/// handle and what it said — `Agent s-…-sub-… · done: 3529`. The heading is dropped, because a
/// line that names its own kind does not need *"a job you backgrounded has ended:"* above it,
/// and a group of three becomes three lines rather than one heading and a count.
///
/// # Only the sentence this head KNOWS is hidden
///
/// **A fold that swallows text it does not recognise is a fold that loses information**, and
/// this one has an obvious way to do it: a notice whose shape changes under it would have its
/// new half silently dropped. So every part has to be accounted for — the opening has to be one
/// of the known ones, every settlement line has to be a bullet (a backticked handle for the two
/// that are collected by an id, the line itself for the plan's nudge), and the only text that may
/// follow them is one of the named promises. Anything else returns `None` and the row is drawn
/// raw, exactly as before this existed.
/// The failure of a future change here is then *"the notices got long again"*, which is visible,
/// rather than *"a notice lost a line"*, which is not.
///
/// **The record is untouched.** This is a rendering: the transcript still holds the daemon's
/// words verbatim, and the model still reads the whole of them. That is also what makes dropping
/// the promise here honest rather than a loss — what the operator cannot see is on the row they
/// are looking at, not deleted from it. Opening it on demand is the half leticl has and this head
/// does not; it is **not** in this change (see TODO.md, the notice-fold row).
pub(crate) fn folded_notice(text: &str, subagents: &[SubagentState]) -> Option<String> {
    /// **The sentences that prove the rest of a row is advice TO THE MODEL rather than a fact
    /// about the thing reported.** One per opening, because a nag's advice is not a completion's
    /// — and named as phrases rather than by position, so a row whose shape changed under this
    /// head stops folding instead of losing the half it did not recognise.
    const PROMISES: &[&str] = &[
        "you do not need to wait for it",
        // The plan's nudge, whose two closings are the operator's row and the model's own
        // (`unfinished_plan`): the first is *do it or mark it done, quoting the text*, the
        // second is *do this one, or mark it done, or drop it*.
        "mark it done with `todo_write`",
        "mark it done, or drop it",
    ];
    let mut out: Vec<String> = Vec::new();
    for group in text.split("\n\n") {
        let (head, rest) = group.split_once('\n')?;
        // **The noun a folded line names itself with, and whether its bullet is a HANDLE or the
        // line itself.** A job and a child are collected by an id — it is what `job_output` and
        // `task_result` take — so their bullets are backticked handles. A todo nag is not
        // collected by anything: its bullet is the item's own words and whose row it is, which is
        // the HEAD the operator asked to see (`in read verbosity todo nag shouldnt show me model
        // prompt only todo head`).
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
                let body = match sep {
                    Some(sep) => {
                        let (handle, said) = bullet.split_once(' ')?;
                        let handle = handle.strip_prefix('`')?.strip_suffix('`')?;
                        // **A child's own task, LOOKED UP rather than parsed.** The notice carries
                        // only what the child answered; what it was asked is on the subagent row,
                        // and a second source for that fact is how the row and the pane come to
                        // disagree. Absent — an older daemon, or a child this head never watched
                        // spawn — the line still names the handle and the answer.
                        let said = if who == "Agent " {
                            match subagents.iter().find(|s| s.session_id == handle) {
                                Some(s) => format!("{} · {said}", subagent_asked(s)),
                                None => said.to_string(),
                            }
                        } else {
                            said.to_string()
                        };
                        format!("{handle}{sep}{said}")
                    }
                    None => bullet.trim().to_string(),
                };
                out.push(format!("{who}{body}"));
            } else if PROMISES.iter().any(|p| line.contains(p)) {
                promised = true;
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
    (!out.is_empty()).then(|| out.join("\n"))
}

pub(crate) fn user_block(text: &str, ts: u64, cfg: &RenderConfig) -> Vec<String> {
    let folded = fold_cells(text);
    // **The renderer sanitises its own input** (§3.1), so a caller cannot forget. The
    // operator's own keystrokes cannot carry a control byte — the decoder hands back
    // `Key::Char` — but a *paste* can, and it arrives here as theirs.
    let clean = without_control_lines(folded.as_deref().unwrap_or(text));
    let text: &str = &clean;
    let p = cfg.palette();
    let w = cfg.width.max(20);
    let bar = p.paint(Role::UserAccent, "▌");
    let stamp = clock_time(ts);
    // The first row shares its width with the timestamp; the rest have the row.
    let head_w = w.saturating_sub(2 + visible_width(&stamp) + usize::from(!stamp.is_empty()));
    let mut lines = wrap(text, head_w.max(8));
    if lines.is_empty() {
        lines.push(String::new());
    }
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        // Padded to the full width so the block is a block: `term::paint` erases
        // each row it rewrites with `\x1b[K`, and a background that stops early
        // leaves a ragged right edge that reads as damage.
        let tail = if i == 0 && !stamp.is_empty() {
            let pad = w
                .saturating_sub(2)
                .saturating_sub(visible_width(l))
                .saturating_sub(visible_width(&stamp));
            format!("{}{stamp}", " ".repeat(pad))
        } else {
            " ".repeat(w.saturating_sub(2).saturating_sub(visible_width(l)))
        };
        out.push(format!(
            "{bar} {}",
            p.paint(Role::UserBlock, &format!("{l}{tail}"))
        ));
    }
    out
}

/// **A `/cells` message, with the screen taken back out of it.**
///
/// The rows are sent for the model and they are a photograph of this head, so
/// rendering them inside this head is a picture of the terminal inside the
/// terminal — re-wrapped to a narrower body, which breaks every box it drew. Worse,
/// it is permanent: the transcript is scrolled back through for the rest of the
/// session.
///
/// So the transcript keeps the operator's words and one line saying what went with
/// them. Nothing is hidden that the line does not name, and the model still has
/// every row. `None` when there is no screen in the text, which is every other
/// message.
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
/// The block sits at the tail of the body — the place its row will occupy the
/// moment the step boundary appends it — so a message typed mid-turn never leaves
/// the screen: it changes from `queued` to a timestamped row in place. Dim text
/// rather than the raised block, because the raised block says "this is in the
/// conversation" and until the boundary it is not; the tag is what says what is
/// true instead, in [`Role::Pending`], the colour the spinner already uses for
/// something in flight.
/// **The mark on an echo: `queued` only while nothing is drawing it.**
///
/// R2, and the operator's own order: *"my message | your line | and only then unqueued."* The reply
/// streams (`Delta` carries its text) while the prompt's row is announced in a frame that carries
/// **no** text, its body following later — so a surface that waits for the body to stop saying
/// `queued` is showing the answer to a question it has not drawn yet.
///
/// **`unconfirmed` is the honest word for that window, and it is not a weakening.** The words are on
/// the screen — a row is drawing them — and what the head still owes is the BODY: whether the row's
/// own text will match what was bound. So the three states stay three: `queued` (nothing has it),
/// `unconfirmed` (a row has it and the body is coming), unmarked (it landed).
///
/// **One function because this was two spellings and they disagreed.** The tail asked
/// `claimed_by.is_some() || unconfirmed` while the WALK drew every bound row as `QUEUED` outright,
/// so fixing one left the other drawing the stale mark — and it was the row's own line the operator
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
    // **A row is drawing these words, so there is nothing left to claim.** `queued` would be
    // false, and `unconfirmed` belongs to the snapshot case above rather than to this one — the
    // head knows exactly what is happening here. The row has landed; its body is still coming, and
    // that is what a row looks like while it is filling in.
    if drawn {
        return "";
    }
    QUEUED
}

pub(crate) fn queued_lines(text: &str, cfg: &RenderConfig, mark: &str, open: bool) -> Vec<String> {
    // Folded here as well as in `user_block`, and it has to be the same text going
    // in: the pending row is removed when the transcript's user item MATCHES it, so
    // a head that queued an abbreviation and received the real thing would leave the
    // `queued` line on the screen for the rest of the session. Measured — the fold
    // belongs to the rendering, not to what was sent.
    let folded = fold_cells(text);
    // **And sanitised here rather than at the two callers** (§3.1), which is the fix
    // for a hole the falsification test found: this echo reached the screen raw while
    // `user_block` — the settled row it becomes — was already guarded. Two callers and
    // one of them forgetting is the same shape as every other leak in this family.
    let clean = without_control_lines(folded.as_deref().unwrap_or(text));
    let text: &str = &clean;
    let p = cfg.palette();
    let w = cfg.width.max(20);
    let bar = p.paint(Role::UserAccent, "▌");
    // The first row shares its width with the tag; the rest hang under the text.
    let head_w = w.saturating_sub(2 + visible_width(mark) + 3);
    let mut lines = wrap(text, head_w.max(8));
    if lines.is_empty() {
        lines.push(String::new());
    }

    // **R33: it is a thing WAITING, not content to read — so it is ONE elided
    // headline.** The operator, looking at three of their own messages queued:
    // *"three giant messages queued"* — a 63-row pane filled with the reader's own
    // words, the conversation pushed off the screen. They typed it; they do not need
    // it read back.
    //
    // The unit of the seam is **screen rows**, not source lines, and that is the same
    // choice `thinking_header` makes: the model writes one enormous paragraph, so
    // "3 lines" beside a fold that opens to half a screen answers the wrong question.
    // What a reader wants to know is how much of the terminal this is about to cost.
    //
    // The key is `/t`, which is this head's *unfold the long rows* verb — one key for
    // one idea, rather than a third fold chord. See `App::echo_open`.
    // **A row that has landed carries no mark**, and then this draws the shape the settled row
    // has — bar and text, no label. Without this the empty mark rendered as a bare ` · ` between
    // the bar and the words, which is a mark saying nothing in the place a mark goes.
    if mark.is_empty() {
        let head_w = w.saturating_sub(2);
        let mut lines = wrap(text, head_w.max(8));
        if lines.is_empty() {
            lines.push(String::new());
        }
        return lines
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                if i == 0 {
                    format!("{bar} {l}")
                } else {
                    format!("  {l}")
                }
            })
            .collect();
    }
    if !open && lines.len() > 1 {
        let seam = format!("  … +{} lines · /t opens it", lines.len() - 1);
        let room = head_w.saturating_sub(visible_width(&seam));
        if room >= 16 {
            return vec![format!(
                "{bar} {}{}{}",
                p.paint(Role::Pending, &format!("{mark} · ")),
                p.paint(Role::Faint, &trim_to(&lines[0], room)),
                p.paint(Role::Faint, &seam),
            )];
        }
        // **A terminal too narrow for the seam still gets one row.** The headline
        // alone, elided by the bar's own width — a seam that does not fit would push
        // the row to two lines and undo the requirement on exactly the screens where
        // it matters most.
        return vec![format!(
            "{bar} {}{}",
            p.paint(Role::Pending, &format!("{mark} · ")),
            p.paint(Role::Faint, &trim_to(&lines[0], head_w.max(8))),
        )];
    }

    let indent = " ".repeat(visible_width(mark) + 3);
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        let label = if i == 0 {
            p.paint(Role::Pending, &format!("{mark} · "))
        } else {
            indent.clone()
        };
        out.push(format!("{bar} {}{}", label, p.paint(Role::Faint, l)));
    }
    out
}

/// `14:32:07` in the local zone, or empty when the row carries no timestamp.
///
/// Zero is *unknown*, not the epoch: a log recorded before `SnapshotItem::ts`
/// existed replays with zeros, and rendering those as `01:00:00` would be a
/// measurement that was never taken rendered as one that was.
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
