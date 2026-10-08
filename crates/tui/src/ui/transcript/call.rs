//! **A tool call's card**: the verb and subject, how it ended, and its body — drawn by
//! `rano::agent::card` from this head's call state.

use crate::app::*;
use crate::ui::render::{RenderConfig, row, row_strings};
use letibot_sessionlog::view::CallState;
use rano::agent::card::{CallState as Live, EditDiff, RawCall, ToolCall, WritingCall};
use rano::agent::decision::{Advice, SettledDecision, Verdict};
use rano::agent::outcome::Outcome;
#[cfg(test)]
use rano::style::Role;

/// The subject helpers the rows share with rano's widgets: one copy of each rule.
pub(crate) use rano::agent::text::ellipsise_left;
#[cfg(test)]
pub(crate) use rano::agent::text::{header_names_the_file, shorten_subject};

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
/// `crates/ui/DESIGN.md` §1: the display side (`rano::agent` now) deliberately does not
/// depend on `letibot-transcript`, so a rendering primitive can be tested without the
/// transcript and a head that wants to draw something the transcript does not
/// model does not have to fork it. The cost is this function, and it is the right
/// place for the cost to land — it is four lines and it is where the two
/// vocabularies are reconciled *once*.
pub(crate) fn display_outcome(o: &letibot_transcript::ToolOutcome) -> Outcome {
    use letibot_transcript::ToolOutcome as O;
    match o {
        O::Ok => Outcome::Ok,
        // §8.2: abstention is not a flavour of success, and `card::Outcome`
        // keeps the distinction for exactly that reason.
        O::Abstained { reason } => Outcome::Abstained(reason.clone()),
        O::Failed { reason } => Outcome::Failed(reason.clone()),
        O::Denied { req_id } => Outcome::Denied(format!("the call was denied ({req_id})")),
        // **Lossless, and it used to collapse three outcomes into `Failed` with a sentence in the
        // reason** — which is how the word changed as a row landed: the live card said
        // `failed · timed out` and the settled row said `timeout`, for one call. See
        // [`Outcome::word`]. The reason is the SENTENCE, and it is this head's: the card owns
        // the word and the head owns what it says about it.
        O::Timeout => Outcome::Timeout,
        O::NotRun { why } => Outcome::NotRun(why.clone()),
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
        } => Outcome::Backgrounded(format!(
            "as `{handle}` after {:.1}s — {next}",
            *ran_for_ms as f64 / 1000.0
        )),
    }
}

/// **How a call ended, in one word** — and there is no longer a second spelling of this in the
/// head. See [`Outcome::word`] for the two that agreed on nothing but the word `failed`, and
/// for why the card is the side that moved.
pub(crate) fn outcome_word(o: &letibot_transcript::ToolOutcome) -> &'static str {
    display_outcome(o).word()
}

/// **The register a settled call's row is drawn in** — R51 item 9.
///
/// Through [`display_outcome`] and then the display side's own [`Outcome::row_role`], so there
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
#[cfg(test)]
pub(crate) fn outcome_role(o: &letibot_transcript::ToolOutcome) -> Role {
    // The one local decision above (`ok` faint) is rano's `row_role` now, beside the mapping
    // it is layered on, so the live card and the settled row read it from one place.
    display_outcome(o).row_role()
}

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
    row(&WritingCall { now_ms }.line(cfg.width), cfg.palette())
}

/// The raw, unparsed text of a tool call, behind `ctrl-x`.
///
/// Rendered as a labelled block rather than inline, because the whole point is
/// that this is *not* the assistant speaking. Faint and fenced: it is evidence,
/// and evidence that looks like prose is how the defect started.
///
/// **Sanitised at the draw, because this is the only place it is drawn** (§3.1): `ctrl-x`
/// shows the model's own markup, and a parameter's value is a string the model chose.
/// `RawCall` cleans it before it is wrapped.
pub(crate) fn raw_call_lines(cfg: &RenderConfig, raw: &str) -> Vec<String> {
    row_strings(
        &RawCall {
            raw: raw.to_string(),
        }
        .lines(cfg.width),
        cfg.palette(),
    )
}

/// **A file edit's before/after, as `rano::agent` takes it**: rano's own edit view (split or
/// unified, the operator's `/diff` toggle) at `width`, and the excerpt's cap.
///
/// **§3.1: a diff's two sides are a file's bytes, and this head did not write them.** The
/// excerpt is read off disk — a build artefact, somebody else's source, a file another
/// process is writing — and a row is written to the terminal verbatim, so an escape in the
/// file is an escape on the operator's terminal. Sanitised **before the diff is taken**, not
/// after it is rendered, so the two sides the differ compares are the two sides the reader
/// sees; a `\r` left in would move the cursor inside a row the renderer had already measured.
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

/// **A settled decision, in rano's words** — who decided, how, and the two reasons a decision
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

/// **What the guard model said**, in rano's words: whether it was consulted at all (R11 — an
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
            Live::Running {
                elapsed_ms: now_ms.saturating_sub(from),
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
    // §4.1: `ToolCallProposed` carries a bounded display target beside the digest — the
    // path, the pattern, the command line — so a call that is still running says `Running
    // "cargo test --workspace"` rather than `Running bash`. Empty is rendered as nothing.
    let call = ToolCall {
        name: c.name.clone(),
        call_id: c.call_id.clone(),
        target: c.target.clone(),
        state,
        // The before/after view, only for the verbs that draw one: the card indents its
        // body by two and the turn block steps the card in by the activity indent after
        // it has rendered, so the panels are built for the width the row will have.
        diff: edit
            .filter(|_| rano::agent::card::Verb::of(&c.name).is_an_edit())
            .map(|e| edit_diff(e, ToolCall::diff_width(cfg.width), cfg, diff_split)),
        decision: c.decision.as_ref().map(settled_decision),
        fold: fold.into(),
    };
    row_strings(&call.lines(cfg.width), cfg.palette())
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
