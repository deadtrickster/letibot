//! **A tool call's card**: the verb and subject, how it ended, and its body - drawn by
//! `rano::agent::card` from this head's call state.

use crate::app::*;
use crate::ui::render::{RenderConfig, row, row_strings};
use letibot_sessionlog::view::CallState;
use rano::agent::card::{CallState as Live, EditDiff, RawCall, ToolCall, WritingCall};
use rano::agent::decision::{Advice, SettledDecision, Verdict};
use rano::agent::outcome::Outcome;
#[cfg(test)]
use rano::style::Role;

/// The subject helpers the rows share with rano's widgets - one copy of each rule, rano's  -
/// named here for the tests that pin them.
#[cfg(test)]
pub(crate) use rano::agent::text::{ellipsise_left, header_names_the_file, shorten_subject};

/// The one mapping from engine types to display types. The display side deliberately does
/// not depend on `letibot-transcript`, so the cost of reconciling the two vocabularies lands
/// here, once, rather than in every rendering primitive.
pub(crate) fn display_outcome(o: &letibot_transcript::ToolOutcome) -> Outcome {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => Outcome::Ok,
        // §8.2: abstention is not a flavour of success, and `card::Outcome`
        // keeps the distinction for exactly that reason.
        O::Abstained { reason } => Outcome::Abstained(reason.clone()),
        O::Failed { reason } => Outcome::Failed(reason.clone()),
        O::Denied { req_id } => Outcome::Denied(format!("the call was denied ({req_id})")),
        // Lossless: folding these into `Failed` with a sentence made the word change as a row
        // landed. The card owns the word, the head owns what it says about it.
        O::Timeout => Outcome::Timeout,
        O::NotRun { why } => Outcome::NotRun(why.clone()),
        // Not `Failed` (a retry offered for a command still working), not `Ok` (that reads as
        // a finish). One sentence with both halves: how long it ran, and the call that reaches
        // its output.
        O::Backgrounded {
            handle,
            ran_for_ms,
            next,
            ..
        } => Outcome::Backgrounded(format!(
            "as `{handle}` after {:.1}s — {next}",
            *ran_for_ms as f64 / 1000.0
        )),
    }
}

/// **How a call ended, in one word** - and there is no longer a second spelling of this in the
/// head. See [`Outcome::word`] for the two that agreed on nothing but the word `failed`, and
/// for why the card is the side that moved.
pub(crate) fn outcome_word(o: &letibot_transcript::ToolOutcome) -> &'static str {
    display_outcome(o).word()
}

/// The register a settled call's row is drawn in: through [`display_outcome`] and then the
/// display side's own [`Outcome::row_role`], so there is ONE outcome-to-register mapping and
/// this row cannot disagree with the card the live call was drawn as.
#[cfg(test)]
pub(crate) fn outcome_role(o: &letibot_transcript::ToolOutcome) -> Role {
    // The one local decision above (`ok` faint) is rano's `row_role` now, beside the mapping
    // it is layered on, so the live card and the settled row read it from one place.
    display_outcome(o).row_role()
}

/// The affordance that stands in for a tool call while the model is writing it: the
/// half-written markup never reaches the screen, and the spinner runs off the log's own clock
/// so a replayed session animates as the live one did.
pub(crate) fn writing_call_line(cfg: &RenderConfig, now_ms: u64) -> String {
    row(&WritingCall { now_ms }.line(cfg.width), cfg.palette())
}

/// The raw, unparsed text of a tool call, behind `ctrl-x`. Fenced and faint because it is
/// evidence rather than the assistant speaking, and sanitised at the draw, this being the only
/// place it is drawn.
pub(crate) fn raw_call_lines(cfg: &RenderConfig, raw: &str) -> Vec<String> {
    row_strings(
        &RawCall {
            raw: raw.to_string(),
        }
        .lines(cfg.width),
        cfg.palette(),
    )
}

/// A file edit's before/after as `rano::agent` takes it: rano's own edit view at `width`.
/// Sanitised before the diff is taken, not after it is rendered, so the two sides the differ
/// compares are the two the reader sees and a `\r` cannot move the cursor inside a row.
pub(crate) fn edit_diff(
    e: &letibot_transcript::ToolEditExcerpt,
    width: usize,
    cfg: &RenderConfig,
    diff_split: bool,
) -> EditDiff {
    use letibot_ui::text::without_control_lines;
    let dcfg = rano::diff::DiffConfig {
        width,
        palette: cfg.palette(),
        // The excerpt already carries ±3 lines of context around the change; re-diffing
        // with the same keeps it intact.
        context: 3,
        line_numbers: true,
        intra_line: false,
        max_rows: 60,
    };
    let path = without_control_lines(&e.path);
    let before = without_control_lines(&e.before);
    let after = without_control_lines(&e.after);
    let rows = rano::sidediff::render_edit_view(
        &path,
        &before,
        &after,
        e.before_start,
        e.after_start,
        &dcfg,
        rano::sidediff::edit_view(diff_split),
    );
    EditDiff {
        path: path.into_owned(),
        rows,
        capped_at: e.truncated.then_some(e.after_lines),
    }
}

/// The tool a row is drawn as: `notes` drawn as the `edit` (or `write`) it is when it hands
/// back a change of its own. `rano` reads the verb from the name, so the plain name drew a
/// `Ran` row that named the file three times and hid the diff.
pub(crate) fn drawn_as<'a>(
    name: &'a str,
    edit: Option<&letibot_transcript::ToolEditExcerpt>,
) -> &'a str {
    match edit {
        Some(e) if name == "notes" && e.created => "write",
        Some(_) if name == "notes" => "edit",
        _ => name,
    }
}

/// The rows a change DETECTED after a command draws: rano draws an [`EditDiff`] only for an
/// edit verb, which is right for the header and wrong for the body. Drawn even while folded,
/// because a diff behind a fold the reader did not know about is no diff.
pub(crate) fn detected_diff_rows(
    e: &letibot_transcript::ToolEditExcerpt,
    target: &str,
    width: usize,
    cfg: &RenderConfig,
    diff_split: bool,
    fold: Fold,
) -> Vec<String> {
    let d = edit_diff(e, width, cfg, diff_split);
    let rows = d.rows_under(target);
    let keep = if fold.is_open() {
        rows.len()
    } else {
        8.min(rows.len())
    };
    let hidden = rows.len() - keep;
    let mut out: Vec<String> = row_strings(&rows[..keep], cfg.palette())
        .into_iter()
        // The two columns rano's own `indent(l, 2)` puts in front of a real edit's rows.
        .map(|l| format!("  {l}"))
        .collect();
    if hidden > 0 {
        out.extend(row_strings(
            &[rano::agent::text::one(
                format!("  … +{hidden} diff rows · /t unfolds it"),
                rano::style::Role::Faint,
            )],
            cfg.palette(),
        ));
    }
    // Folded, the row says how the diff was obtained itself: the tool's sentence is a `[note]`
    // in the payload, a folded row shows the payload's FIRST line only, and any note ahead of
    // it hides the provenance.
    if !fold.is_open() {
        out.extend(row_strings(
            &[rano::agent::text::one(
                "  detected afterwards — the command changed this file; not made by `edit`",
                rano::style::Role::Faint,
            )],
            cfg.palette(),
        ));
    }
    out
}

/// **A settled decision, in rano's words** - who decided, how, and the two reasons a decision
/// carries (the decider's basis, and the guard model's advice when one was asked).
pub(crate) fn settled_decision(d: &letibot_sessionlog::view::SettledDecision) -> SettledDecision {
    use letibot_sessionlog::event::DecisionOutcome as O;
    SettledDecision {
        verdict: match &d.outcome {
            O::Selected { option_id } => Verdict::of_option(option_id),
            O::Cancelled => Verdict::Cancelled,
            O::TimedOut => Verdict::NotAnswered,
        },
        by_kind: d.by.kind.clone(),
        by_identity: d.by.identity.clone(),
        summary: d.summary.clone(),
        basis: d.basis.clone(),
        advice: d.advice.as_ref().map(model_advice),
    }
}

/// **What the guard model said**, in rano's words: whether it was consulted at all (R11 - an
/// unconsulted verdict is nobody speaking), who, how fast, what it would do and why, what it
/// cited, and the non-answer it gave when it gave one (R12).
pub(crate) fn model_advice(a: &letibot_sessionlog::event::ModelAdvice) -> Advice {
    Advice {
        consulted: a.consulted,
        by: a.by.clone(),
        latency_ms: a.latency_ms,
        would: a.would.clone(),
        basis: a.basis.clone(),
        cites: a.cites.clone(),
        unsure: a.unsure.clone(),
    }
}

/// One live tool call, as a card: how long it has been running, what it last said, and what
/// happened to the rest of a spill. The body is empty while a call is in flight, because a
/// finished call carries digests and the payload arrives only through the transcript.
pub(crate) fn call_card(
    c: &CallRow,
    cfg: &RenderConfig,
    now_ms: u64,
    fold: Fold,
    diff_split: bool,
) -> Vec<String> {
    let mut edit = None;
    let state = match &c.state {
        CallState::Proposed => Live::Proposed {
            note: c.note.clone(),
        },
        CallState::Running => {
            // The clock the caller chose, and the anchor that goes with it: subtracting
            // `started_ms` from the head's clock would mix the two frames and report a smaller
            // number, which reads like progress.
            let from = if c.started_at > 0 {
                c.started_at
            } else {
                c.started_ms
            };
            Live::Running {
                // No anchor at all (a replay before the clock is set) shows zero, not the epoch.
                elapsed_ms: if from == 0 {
                    0
                } else {
                    now_ms.saturating_sub(from)
                },
                note: c.note.clone(),
            }
        }
        CallState::Finished {
            outcome,
            inline_bytes,
            full_bytes,
            spill,
            edit: e,
            ..
        } => {
            edit = e.as_ref();
            Live::Finished {
                outcome: display_outcome(outcome),
                // A snapshot has no timestamps, and `0.0s` is a measurement that was never
                // taken rendered as one that was.
                elapsed_ms: (c.started_ms != 0 && c.ended_ms != 0)
                    .then(|| c.ended_ms.saturating_sub(c.started_ms)),
                inline_bytes: *inline_bytes,
                full_bytes: *full_bytes,
                spill: spill.clone(),
            }
        }
    };
    // §4.1: `ToolCallProposed` carries a bounded display target beside the digest - the
    // path, the pattern, the command line - so a call that is still running says `Running
    // "cargo test --workspace"` rather than `Running bash`. Empty is rendered as nothing.
    let name = drawn_as(&c.name, edit);
    let verb = rano::agent::card::Verb::of(name);
    let call = ToolCall {
        name: name.to_string(),
        call_id: c.call_id.clone(),
        // An edit card names its file, and a note's target is its stem and its text.
        target: match edit {
            Some(e) if name != c.name => e.path.clone(),
            _ => c.target.clone(),
        },
        state,
        // The before/after view, only for the verbs that draw one: the card indents its
        // body by two and the turn block steps the card in by the activity indent after
        // it has rendered, so the panels are built for the width the row will have.
        diff: edit
            .filter(|_| verb.is_an_edit())
            .map(|e| edit_diff(e, ToolCall::diff_width(cfg.width), cfg, diff_split)),
        decision: c.decision.as_ref().map(settled_decision),
        fold: fold.into(),
    };
    let mut out = row_strings(&call.lines(cfg.width), cfg.palette());
    // The same panel the settled row draws, for the same reason: a diff that appeared only
    // when the card settled would flicker one frame with the change and one without.
    if let Some(e) = edit.filter(|_| !verb.is_an_edit()) {
        out.extend(detected_diff_rows(
            e,
            &c.target,
            ToolCall::diff_width(cfg.width),
            cfg,
            diff_split,
            fold,
        ));
    }
    out
}

/// Why it ended that way, when there is a why. Through [`display_outcome`] and then the card,
/// so this row's reason and the live card's are one string for one call rather than two
/// spellings that disagree.
#[cfg(test)]
pub(crate) fn outcome_why(o: &letibot_transcript::ToolOutcome) -> Option<String> {
    display_outcome(o).reason().map(str::to_string)
}

/// A path with `$HOME` written as `~`. Twelve columns of an eighty-column header
/// spent on `/home/dead` is twelve columns not spent on the session's name.
pub(crate) fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}
