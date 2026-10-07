//! **The notes**: the disclosures this head has shown, folded and unfolded (`/notes`).

use crate::app::*;
use crate::render::{RenderConfig, sgr, wrap};
use crate::ui::*;
use letibot_ui::text::without_control_lines;

impl App {
    /// The `/notes` listing: every note this head holds, in the order the
    /// conversation has them, numbered for `/notes dismiss N`.
    ///
    /// **Including the ones it is not drawing** (R19). A note from before this window is a
    /// fact this head holds and has chosen not to plant in the conversation, so the
    /// listing is where a reader finds it — marked, for the same reason a retired one is:
    /// two different reasons for an absence must not look like one.
    pub(crate) fn notes_lines(&self) -> Vec<String> {
        let n = self.notes.len();
        let retired = self.retired_notes();
        let before = self.notes_before();
        let mut out = if n == 0 {
            vec![
                "this head holds no notes. A note is a disclosure: a guard that fired, a \
                 decision that settled, a sentence the daemon interrupted with. They are \
                 in the session log whether or not this head is showing them."
                    .to_string(),
            ]
        } else {
            vec![format!(
                "{n} note(s), {retired} retired{} — the log holds the durable fact; a note is \
                 how a head shows it once",
                if before == 0 {
                    String::new()
                } else {
                    format!(", {before} from before this window")
                }
            )]
        };
        for (i, (place, note)) in self.notes.iter().enumerate() {
            // **The same renderer the transcript uses, unfolded.** A listing that hid
            // the tail of the very thing it exists to make findable would be the
            // defect again, so the whole text is here.
            let body = note_lines_unfolded(&self.cfg, note);
            let mut marks: Vec<&str> = Vec::new();
            if self.is_retired(note) {
                marks.push("retired");
            }
            if matches!(place, Placed::Before) {
                marks.push("before this window");
            }
            let mark = if marks.is_empty() {
                String::new()
            } else {
                format!("[{}]", marks.join(", "))
            };
            out.push(format!("{:>3}  {mark}", i + 1));
            out.extend(body);
        }
        if n > 0 {
            out.push(String::new());
            out.push(
                "/notes dismiss [N|all] retires one, or every one · /notes restore brings \
                 them all back"
                    .to_string(),
            );
        }
        out
    }
}

/// **How many lines of a note the conversation shows before it is a wall.**
///
/// R10's other half, and the number comes from the operator's own screen rather
/// than from taste: two gate timeouts rendered **27 red lines** (*"how to remove
/// this red wall?"*), which is around thirteen lines each — a `denied:` detail with
/// the whole rule in it. Three lines keeps the code, the first sentence and the
/// fact that there is more, and puts the rest one verb away; `/notes` prints the
/// whole thing, so this is a disclosure decision and never a cap on the record.
///
/// The instrument is the one every other long thing in this head already uses —
/// `keep = 8` for a card's diff rows, a seam row naming where the rest is — because
/// a warning is not more important for being longer.
pub(crate) const NOTE_LINES: usize = 3;

/// One note, as the transcript draws it: at most [`NOTE_LINES`] lines and a seam.
pub(crate) fn note_lines(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    // **A pane's ending is the one note that is not folded**, and the reason is the tail: what
    // a program says as it dies is the *last* thing it printed, so a fold that kept the first
    // three lines would keep the least useful three. The length is bounded where it is
    // captured instead — [`PANE_LAST_LINES`] — so this cannot become a wall.
    if matches!(n, Note::Pane { .. }) {
        return note_lines_unfolded(cfg, n);
    }
    let all = note_lines_unfolded(cfg, n);
    if all.len() <= NOTE_LINES {
        return all;
    }
    let hidden = all.len() - NOTE_LINES;
    let mut out: Vec<String> = all.into_iter().take(NOTE_LINES).collect();
    out.push(dim(cfg, &format!("  … +{hidden} lines · /notes")));
    out
}

/// **How many of a pane's last rows its ending carries** — the cap that replaces the fold.
///
/// See [`note_lines`]: a pane's ending is not folded to [`NOTE_LINES`], so it needs a bound of
/// its own, and four is chosen the way `NOTE_LINES` was: the common case is one row (a program
/// that dies with a sentence about why), a full-screen program leaves a whole rectangle, and a
/// rectangle is not what a transcript row is for. What is past the cap is *not* kept anywhere —
/// a pane's bytes are not recorded, see `harnessd`'s own TODO — so this is a disclosure
/// decision and not a fold over a record.
pub(crate) const PANE_LAST_LINES: usize = 4;

/// The whole note, with no fold — what `/notes` lists and what the transcript
/// shows the head of. One renderer for both, so the listing cannot disagree with
/// the screen about the text.
pub(crate) fn note_lines_unfolded(cfg: &RenderConfig, n: &Note) -> Vec<String> {
    // **§3.1: a note carries text from elsewhere.** A `Warning`'s detail is the
    // daemon's or a guard's sentence, a `Decided`'s summary and basis are the ask and
    // the decider's own words — and all of it lands in rows `paint_full` writes
    // verbatim, so all of it is sanitised here. This is the renderer for both the
    // transcript row and `/notes`, which is why it is one place and not two.
    match n {
        Note::Warned(w) => {
            // **The code decides the register, and the code is the log's word rather than
            // this head's** (`letibot_sessionlog::warning`, R19 and R29 part two).
            //
            // Three registers, and each is a claim about where the reader's attention should
            // be. The mark goes with the colour, because colour is a no-op under
            // `--replay`, a pipe and a light theme — R20's own argument — and three
            // registers that collapse to one appearance in half the terminals they are read
            // in are one register with extra steps.
            //
            // ```text
            //   · code — sentence      dim      housekeeping: nothing to do
            //   × code — sentence      notice   the answer to what you just typed; retype
            //   ! code — sentence      red      the session is in trouble; stop and look
            // ```
            //
            // The middle one is R29 part two, and the reasoning for a third register rather
            // than two is in `Class`'s own docs: seven codes of forty-six, and they are the
            // seven a reader meets *most often* because they fire while they are typing.
            use letibot_sessionlog::warning::Class;
            let class = letibot_sessionlog::warning::class(&w.code);
            let (mark, colour) = match class {
                Class::Routine => ("·", sgr::DIM),
                Class::Refused => ("×", sgr::YELLOW),
                Class::Failure => ("!", sgr::RED),
            };
            let said = format!("{mark} {} — {}", w.code, w.detail);
            wrap(&without_control_lines(&said), cfg.width)
                .into_iter()
                .map(|l| note_line(cfg, colour, &l))
                .collect()
        }
        // No `!`, no red, no request id: nothing here is answerable, and the id is
        // only useful to somebody typing a grant. The detail that was cut is not
        // lost — the model's own tool result carries it, folded, one row above.
        Note::NotRun(w) => wrap(
            &without_control_lines(&format!("· {}", w.detail)),
            cfg.width,
        )
        .into_iter()
        .map(|l| dim(cfg, &l))
        .collect(),
        Note::Pane {
            line,
            said,
            reason,
            closed,
        } => {
            // **The register is the operator's own act, and it is a fact this head holds.**
            // A pane they ENDED deliberately — `!term close`, confirmed — is housekeeping: dim,
            // a `·`. A pane that ended without them is the answer to what they just typed: the
            // notice register, a `×`. Reading that out of the reason's *wording* would be a head
            // matching on a sentence the daemon composes; `closed` is the head's own record that
            // it sent `Action::TermClose`.
            //
            // **A detach is neither**, and it never reaches here: leaving with `ctrl-\` files
            // no row at all, because it ends nothing — see [`TermPane`].
            let (mark, tint) = if *closed {
                ("·", sgr::DIM)
            } else {
                ("×", sgr::YELLOW)
            };
            let mut rows: Vec<String> = Vec::with_capacity(said.len() + 1);
            rows.push(format!("{mark} {line} — {reason}"));
            // The program's own rows, indented under the line that ran it — and sanitised,
            // because a cell holds anything the parser let through.
            rows.extend(said.iter().map(|l| format!("    {l}")));
            rows.iter()
                .flat_map(|l| wrap(&without_control_lines(l), cfg.width))
                .map(|l| colour(cfg, tint, &l))
                .collect()
        }
        Note::Decided(d) => {
            use letibot_sessionlog::event::DecisionOutcome as O;
            let (word, code) = match &d.outcome {
                O::Selected { option_id } if option_id.starts_with("allow") => {
                    (format!("allowed ({option_id})"), sgr::GREEN)
                }
                O::Selected { option_id } => (format!("REFUSED ({option_id})"), sgr::RED),
                O::Cancelled => ("cancelled".to_string(), sgr::YELLOW),
                // A deadline is not an answer, and must not read like one.
                O::TimedOut => (
                    "NOT ANSWERED — the deadline decided it".to_string(),
                    sgr::RED,
                ),
            };
            let who = if d.by.identity.is_empty() {
                d.by.kind.clone()
            } else {
                format!("{} {}", d.by.kind, d.by.identity)
            };
            let late = if d.late {
                " · an answer arrived after it had settled"
            } else {
                ""
            };
            wrap(
                &without_control_lines(&format!(
                    "? {} — {word}, by {who}{}{late}",
                    d.summary,
                    if d.basis.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", d.basis)
                    }
                )),
                cfg.width,
            )
            .into_iter()
            .map(|l| colour(cfg, code, &l))
            .collect()
        }
    }
}
