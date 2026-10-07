//! **A tool call's card**: the verb and subject, how it ended, and its body.

use crate::app::*;
use crate::render::{RenderConfig, bytes_human, trim_to, visible_width, wrap};
use crate::ui::*;
use letibot_sessionlog::view::CallState;
use letibot_ui::style::Role;
use letibot_ui::text::without_control_lines;
use letibot_ui::{card, diff::DiffConfig, sidediff};

/// The pane's word for how a job came to be in the background — the three causes
/// `Backgrounding` names, as a person reads them. The distinction is the one the
/// outcome already draws: who wanted it there.
pub(crate) fn how_word(how: &letibot_transcript::Backgrounding) -> String {
    match how {
        letibot_transcript::Backgrounding::Asked => "asked".into(),
        letibot_transcript::Backgrounding::Promoted => "promoted".into(),
        letibot_transcript::Backgrounding::Operator { identity } => {
            format!("promoted by {identity}")
        }
    }
}

/// The one mapping from engine types to display types.
///
/// `crates/ui/DESIGN.md` §1: the display crate deliberately does not depend on
/// `letibot-transcript`, so a rendering primitive can be tested without the
/// transcript and a head that wants to draw something the transcript does not
/// model does not have to fork it. The cost is this function, and it is the right
/// place for the cost to land — it is four lines and it is where the two
/// vocabularies are reconciled *once*.
pub(crate) fn display_outcome(o: &letibot_transcript::ToolOutcome) -> card::Outcome {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => card::Outcome::Ok,
        // §8.2: abstention is not a flavour of success, and `card::Outcome`
        // keeps the distinction for exactly that reason.
        O::Abstained { reason } => card::Outcome::Abstained(reason.clone()),
        O::Failed { reason } => card::Outcome::Failed(reason.clone()),
        O::Denied { req_id } => card::Outcome::Denied(format!("the call was denied ({req_id})")),
        // **Lossless, and it used to collapse three outcomes into `Failed` with a sentence in the
        // reason** — which is how the word changed as a row landed: the live card said
        // `failed · timed out` and the settled row said `timeout`, for one call. See
        // [`card::Outcome::word`]. The reason is the SENTENCE, and it is this head's: the card owns
        // the word and the head owns what it says about it.
        O::Timeout => card::Outcome::Timeout,
        O::NotRun { why } => card::Outcome::NotRun(why.clone()),
        // Not `Failed`, which would put a retry in front of the operator for a
        // command that is still working, and not `Ok`, which would read as a
        // finish. The handle is in the reason because the handle is what makes
        // it reachable.
        // **One sentence, carrying both halves the two spellings had apart.** The card said *in the
        // background as `j4` after 0.4s* — and `ran_for_ms`' own docstring says why that number is
        // there: *"the number that makes a promotion legible rather than mysterious."* The
        // transcript said *as `j4` — /job j4 out*, and `next`' own docstring says why THAT is
        // there: *"the call that gets its output, ready to make. 'Errors carry the fix', applied to
        // something that is not an error."* Neither is decoration, so the one sentence keeps both,
        // in the transcript's shape because that is the one the operator has been reading here.
        O::Backgrounded {
            handle,
            ran_for_ms,
            next,
            ..
        } => card::Outcome::Backgrounded(format!(
            "as `{handle}` after {:.1}s — {next}",
            *ran_for_ms as f64 / 1000.0
        )),
    }
}

/// **How a call ended, in one word** — and there is no longer a second spelling of this in the
/// head. See [`card::Outcome::word`] for the two that agreed on nothing but the word `failed`, and
/// for why the card is the side that moved.
pub(crate) fn outcome_word(o: &letibot_transcript::ToolOutcome) -> &str {
    display_outcome(o).word()
}

/// **The register a settled call's row is drawn in** — R51 item 9.
///
/// Through [`display_outcome`] and then the display crate's own [`card::Outcome::role`], so there
/// is ONE outcome→register mapping in this tree and this row cannot disagree with the card the
/// live call was drawn as.
///
/// **What it replaces.** The row asked its own question, `bad = !matches!(outcome, Ok)`, and drew
/// every non-`ok` outcome in `Failure`. So a `backgrounded` call — a process still working, with a
/// handle to reach it — was red, and read as something to retry, and a `denied` or `abstained` call
/// read as a malfunction rather than as a decision. The operator, looking at the backgrounding
/// message: *"why on earth backgrounding message is in red"*.
///
/// **One local decision layered on the mapping, and it is this head's own.** `ok` is drawn FAINT
/// rather than `Success`: a green line under every command is a colour that says nothing, and the
/// boring case is most of them. That is a decision about one row rather than about the mapping —
/// leticl keeps it faint for the same reason — so it is applied here, on top, and never by
/// rewriting the mapping.
pub(crate) fn outcome_role(o: &letibot_transcript::ToolOutcome) -> Role {
    match o {
        letibot_transcript::ToolOutcome::Ok => Role::Faint,
        other => display_outcome(other).role(),
    }
}

/// One live tool call, as a card.
///
/// This was `call_line`, which produced `● edit(call_7) — ok · 214 B` and could
/// produce nothing else. A card keeps the disclosure and adds the three things a
/// person watching a call is actually looking for: how long it has been running,
/// what it last said, and — for a spill — what happened to the rest of the
/// output.
///
/// **The body is empty while a call is in flight, and that is deliberate.**
/// `ToolFinished` carries digests and byte counts, never a payload; the payload
/// reaches a head only through `TranscriptItem::ToolResult` in a snapshot or a
/// reconciliation. See `crates/ui/DESIGN.md` §4.2 — it is a defensible design (an
/// event fans out to every head; a 480 KB payload should not) and it is why
/// `Phase` distinguishes running from settled at all.
/// The affordance that stands in for a tool call while the model is writing it.
///
/// The defect this replaces: *"tool calls — i see `<function…` like strings first,
/// then closing tag arrives and it becomes a toolcall."* The markup was being
/// rendered as prose because it arrived as prose, which is fixed one layer down
/// (`DeltaTarget::ToolCall`). What is left is the question that markup was
/// accidentally answering — *is something happening?* — and this answers it
/// without showing anybody a half-written `<parameter=`.
///
/// The spinner is driven off the log's own clock, like every other moving thing
/// here, so a replayed session animates the same way the live one did.
pub(crate) fn writing_call_line(cfg: &RenderConfig, now_ms: u64) -> String {
    let p = cfg.palette();
    let spin = letibot_ui::progress::spinner(now_ms).to_string();
    trim_to(
        &format!(
            "{} {}{}",
            p.paint(Role::Pending, &spin),
            p.paint(Role::Pending, "writing a tool call"),
            p.paint(Role::Faint, " · ctrl-x for the raw form")
        ),
        cfg.width,
    )
}

/// The raw, unparsed text of a tool call, behind `ctrl-x`.
///
/// Rendered as a labelled block rather than inline, because the whole point is
/// that this is *not* the assistant speaking. Faint and fenced: it is evidence,
/// and evidence that looks like prose is how the defect started.
pub(crate) fn raw_call_lines(cfg: &RenderConfig, raw: &str) -> Vec<String> {
    let p = cfg.palette();
    let mut out = vec![p.paint(Role::Faint, "┌─ raw tool call · ctrl-x")];
    // **Sanitised at the draw, because this is the only place it is drawn** (§3.1).
    // `ctrl-x` shows the model's own markup — `<function=…><parameter=…>` — and a
    // parameter's value is a string the model chose, so this is one more surface where
    // content this head did not author would otherwise reach the terminal. Kept out of
    // the `Delta` arm on purpose: `raw_call` is accumulated for the whole turn and
    // nothing else reads it, so the one renderer is the right seam.
    let raw = without_control_lines(raw);
    for l in raw.lines() {
        for w in wrap(l, cfg.width.saturating_sub(2)) {
            out.push(format!(
                "{}{}",
                p.paint(Role::Faint, "│ "),
                p.paint(Role::Code, &w)
            ));
        }
    }
    out.push(p.paint(Role::Faint, "└─"));
    out
}

pub(crate) fn call_card(
    c: &CallRow,
    cfg: &RenderConfig,
    now_ms: u64,
    fold: Fold,
    diff_split: bool,
) -> Vec<String> {
    let mut card = card::Card::new(&c.name, &c.call_id);
    // §4.1, fixed. `ToolCallProposed` now carries a bounded display target beside
    // the digest — the path, the pattern, the command line — so a call that is
    // still running says `Running "cargo test --workspace"` rather than `Running
    // bash`. Empty is still possible (a call first seen as `ToolStarted`, or a log
    // recorded before the field existed) and is still rendered as nothing: a digest
    // is not a display string and a guess is worse than a blank.
    card.target = c.target.clone();
    let mut body: Vec<String> = Vec::new();
    // Both sides of the file this call changed, when it changed one and the
    // event carried them. Bound in the arm, used after it: the phase match
    // decides what the header says, and the body decision needs both.
    let mut edit_excerpt: Option<letibot_sessionlog::event::ToolEdit> = None;
    card.phase = match &c.state {
        CallState::Proposed => card::Phase::Proposed {
            note: c.note.clone(),
        },
        CallState::Running => {
            // **The clock the caller chose, and the anchor that goes with it**
            // (R13). `call_card` is handed *one* clock: the head's when this call
            // has an anchor, the log's when it does not (see the pane). Subtracting
            // `started_ms` either way would mix the two frames — the log's start
            // against the head's now — and report the time since the `ToolStarted`
            // event rather than since the call started, which is a *smaller* number
            // and so reads like progress.
            let from = if c.started_at > 0 {
                c.started_at
            } else {
                c.started_ms
            };
            card::Phase::Running {
                elapsed_ms: now_ms.saturating_sub(from),
                note: c.note.clone(),
            }
        }
        CallState::Finished {
            outcome,
            inline_bytes,
            full_bytes,
            spill,
            edit,
            ..
        } => {
            // §8.3's disclosure, as prose and in units a person reads. It goes in
            // the body rather than the header tail because the header tail is
            // dropped whole when it does not fit, and "there is more, and here is
            // how to get it" is not a line that may vanish on a narrow terminal.
            if let Some(hash) = spill {
                body.push(format!(
                    "{} of {} went to the model, the rest is kept — read_spill hash={hash}",
                    bytes_human(*inline_bytes),
                    bytes_human(*full_bytes),
                ));
            } else {
                card.bytes = Some((*inline_bytes, *inline_bytes));
                body.push(bytes_human(*inline_bytes));
            }
            edit_excerpt = edit.clone();
            let outcome = display_outcome(outcome);
            if c.started_ms == 0 || c.ended_ms == 0 {
                // A snapshot has no timestamps, and `0.0s` is a measurement that
                // was never taken rendered as one that was.
                card::Phase::Replayed { outcome }
            } else {
                card::Phase::Finished {
                    outcome,
                    elapsed_ms: Some(c.ended_ms.saturating_sub(c.started_ms)),
                }
            }
        }
    };
    // The before/after view. Split or unified is the operator's `/diff` toggle
    // and nothing else — no width gate, because the two answers a width gate
    // ever gave were a cramped diff or no diff at all. Every other case keeps
    // exactly what the card already said, which is what makes the toggle safe
    // to flip at any width.
    if matches!(card.verb, card::Verb::Edit | card::Verb::Write)
        && let Some(e) = edit_excerpt
    {
        let view = sidediff::edit_view(diff_split);
        let dcfg = DiffConfig {
            // The card indents its body by two, and the turn block steps the
            // whole card in by the activity indent *after* the card has
            // rendered, so the panels are built for the width the row will
            // actually have — or the frame trims the right panel's tail off
            // and the diff lies by omission. The transcript's own diff arm
            // does the same arithmetic at its `let w`.
            width: cfg.width.saturating_sub(2 + activity_indent(cfg.width)),
            palette: cfg.palette(),
            // The excerpt already carries ±3 lines of context around the
            // change; re-diffing with the same keeps it intact.
            context: 3,
            line_numbers: true,
            intra_line: false,
            max_rows: 60,
        };
        // **§3.1: a diff's two sides are a file's bytes, and this head did not
        // write them.** The excerpt is read off disk — a build artefact, somebody
        // else's source, a file another process is writing — and `paint_full`
        // writes the row verbatim, so an escape in the file is an escape on the
        // operator's terminal. Sanitised **before the diff is taken**, not after it
        // is rendered, so the two sides the differ compares are the two sides the
        // reader sees; a `\r` left in would move the cursor inside a row the
        // renderer had already measured.
        let path = without_control_lines(&e.path);
        let before = without_control_lines(&e.before);
        let after = without_control_lines(&e.after);
        body = sidediff::render_edit_view(
            &path,
            &before,
            &after,
            e.before_start,
            e.after_start,
            &dcfg,
            view,
        );
        if header_names_the_file(&card.target, &path) && !body.is_empty() {
            body.remove(0);
        }
        if e.truncated {
            body.push(cfg.palette().paint(
                Role::Faint,
                &format!(
                    "… the excerpt was capped; the file is {} lines now",
                    e.after_lines
                ),
            ));
        }
    }
    // The decision this call was gated by, in the dim register: the approval is a
    // fact about the call, not a stray note. Folded it is one line — who decided
    // and how; open it adds what the oracle was shown and what it said back.
    if let Some(d) = &c.decision {
        use letibot_sessionlog::event::DecisionOutcome as O;
        let word = match &d.outcome {
            O::Selected { option_id } if option_id.starts_with("allow") => "allowed",
            O::Selected { .. } => "refused",
            O::Cancelled => "cancelled",
            O::TimedOut => "not answered",
        };
        let who = if d.by.identity.is_empty() {
            d.by.kind.clone()
        } else {
            format!("{} {}", d.by.kind, d.by.identity)
        };
        body.push(
            cfg.palette()
                .paint(Role::Faint, &format!("· {word}, by {who}")),
        );
        if fold.is_open() {
            for l in decision_detail(d, cfg.width.saturating_sub(4)) {
                body.push(cfg.palette().paint(Role::Faint, &format!("  {l}")));
            }
        }
    }
    card.body = body;
    let verb = card.verb.clone();
    card.render(&card::CardConfig {
        width: cfg.width,
        palette: cfg.palette(),
        // Never `Collapsed`: the spill disclosure lives in the body and a fold is
        // not a licence to hide it.
        mode: match fold {
            Fold::Open => card::DisplayMode::Expanded,
            Fold::Folded => card::DisplayMode::Truncated,
        },
        budget: card::Budget::for_verb(&verb),
        show_id: false,
    })
}

/// Why it ended that way, when there is a why. Goes in the body, where it wraps.
///
/// **Through [`display_outcome`] and then the card**, so this row's reason and the live card's are
/// one string for one call. It was a second list of sentences, and it disagreed with the card's on
/// two of the seven outcomes: `denied, req_1` against `the call was denied (req_1)`, and the
/// backgrounded sentence, where each spelling carried a fact the other dropped — see
/// `display_outcome`, which now builds one sentence out of both rather than picking a winner. Where
/// the two simply disagreed about phrasing, the transcript's is kept: it is the one the operator has
/// been reading on this row all along, and the card is the newer surface.
pub(crate) fn outcome_why(o: &letibot_transcript::ToolOutcome) -> Option<String> {
    display_outcome(o).reason().map(str::to_string)
}

/// Shorten a path to `max` columns by eating its **left**.
///
/// `…/worktrees/agent-a19da2/crates/tui`, not `~/Projects/letibot/.claud…`. A path
/// is recognised by where it ends; truncating from the right of a deep tree leaves
/// every session on this box looking identical.
pub(crate) fn ellipsise_left(s: &str, max: usize) -> String {
    if visible_width(s) <= max || max < 2 {
        return s.to_string();
    }
    // **At a separator, not at a character.** `…/1f0655c6-…/scratchpad` was the
    // operator's example and it is two lies in twenty-two columns: the first
    // ellipsis says a prefix was dropped, which is true, and the second says a
    // directory has a shorter name than it does, which is not — and neither
    // segment can be pasted back into a shell. Dropping *whole* segments leaves a
    // suffix that is a real path, which is what a person compares against.
    //
    // `match_indices` runs left to right, so the first candidate that fits is the
    // longest suffix that fits.
    if s.contains('/') {
        for (i, _) in s.match_indices('/') {
            let cand = format!("…{}", &s[i..]);
            if visible_width(&cand) <= max {
                return cand;
            }
        }
    }
    // A single segment longer than the whole allowance, or no separator at all.
    // Then there is nothing to cut on and the characters are all there is.
    let keep = max - 1;
    let mut out = String::new();
    let mut cols = 0usize;
    for c in s.chars().rev() {
        let cw = visible_width(&c.to_string());
        if cols + cw > keep {
            break;
        }
        out.push(c);
        cols += cw;
    }
    format!("…{}", out.chars().rev().collect::<String>())
}

/// Shorten a tool call's subject to `max` columns, cutting at the end a reader
/// does not need.
///
/// A **path** loses its left, at a separator: `…/crates/tui/src/app.rs` is still
/// a file you can recognise and `crates/tui/src/ap…` is not. Anything else — a
/// regex, a command line, a glob — loses its **right**, because those are read
/// from the start and the first token is the one that says what it is.
///
/// The test for "path" is a separator **and no glob metacharacter**. Measured at
/// 60 columns: `**/*.{md,json,toml,yaml,yml} 40` has a slash in it and cutting
/// its left gave `…json,toml,yaml,yml} 40`, which has lost the fact that it is a
/// glob at all. Cutting its right gives `**/*.{md,json,tom…`, which has not.
/// **Does the row's header already name the file its diff is of?** Then the diff's own name
/// line (`sidediff::render_edit_view`'s first row) says it a second time — the operator, on
/// `▸ Wrote …/pr-body-align.md · ok` with the same path on the line under it: *"why two
/// times?"*. That line exists for the card whose header could NOT name the file (a call id in
/// its place — *"sometimes your Edited card doesnt have file name"*, 2026-10-05), and it stays
/// for that one. A relative target the excerpt's absolute path ends in is the same file.
pub(crate) fn header_names_the_file(target: &str, path: &str) -> bool {
    let t = target.trim();
    !t.is_empty() && (t == path || path.ends_with(&format!("/{t}")))
}

pub(crate) fn shorten_subject(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    // A glob metacharacter, or a quote — `display_target` quotes any argument
    // containing whitespace, so a leading `"` is how prose announces itself.
    // Measured: an `ask_code` call whose subject was a sentence with `src/` in the
    // middle of it left-cut to `…/ is responsible for, how main.rs, editor.rs,
    // and…`, which has thrown away the question and kept its tail.
    let not_a_path = s.contains(['*', '?', '{', '[', '"']);
    if s.contains('/') && !not_a_path {
        ellipsise_left(s, max)
    } else {
        trim_to(s, max)
    }
}

/// A path with `$HOME` written as `~`. Twelve columns of an eighty-column header
/// spent on `/home/dead` is twelve columns not spent on the session's name.
pub(crate) fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}
