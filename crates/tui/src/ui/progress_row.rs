//! **The long waits, as one line of the status row**: a fill the daemon named, a fold, and a
//! snapshot's announced rows still arriving.
//!
//! The operator, 2026-10-14, after the transients moved into the status row (516c5a4): *"yes,
//! move the compacting progress blocks too"*. Each of these was a block of two or three rows at the
//! foot of the conversation — a blank, a bar with the cat, a sentence — that appeared when the
//! operation started, changed height when a fold went from reading to writing, and went away when
//! it ended: three jumps of the whole conversation per operation, on the operations that last
//! longest. Here each is ONE line — label · bar · numbers — drawn in the row the fit ladder
//! reserves whether or not anything is in it, so nothing above the composer moves for them.
//!
//! What the blocks said and the line has no room for is kept: the row is a click target while it
//! leaves anything out, and the click opens the whole of it ([`Progress::detail`]).

use crate::app::*;
use crate::ui::loading::{BODY_PATIENCE, MIN_FILLING};
use crate::ui::render::{trim_to, visible_width};
use rano::agent::header::thousands;
use rano::agent::text::duration;
use rano::agent::turn_status::{Prefill, bar};
use rano::render::{Line, Role, Span};

/// The bar's width in the row, the end caps included: wide enough to move visibly over a minute's
/// carry, narrow enough to leave the label and the numbers room on an 80-column terminal.
pub(crate) const PROGRESS_BAR_COLS: usize = 12;

/// **One progress, in pieces the row can give up one at a time**: the advice first, then the
/// clock, then the counts, then the bar and its percentage — and never the label.
#[derive(Debug, Clone)]
pub(crate) struct Progress {
    /// What is happening, in the words of whoever is doing it. Never dropped.
    pub(crate) label: String,
    /// The label's colour: in flight, or needing a person.
    pub(crate) role: Role,
    /// How far along, when there is a total to be a fraction of. A fold that is writing has none,
    /// and a bar that invented one would be an indicator that is not the fact.
    pub(crate) bar: Option<Prefill>,
    /// The counts, in the operation's own unit.
    pub(crate) counts: Option<String>,
    /// What to do about it, last on the row and the first piece given up.
    pub(crate) advice: Option<String>,
    /// When it started, on this head's clock — the row's clock, and the order between two.
    pub(crate) since_ms: u64,
    /// Everything the block used to say, for the click.
    pub(crate) detail: String,
    /// The block carried prose the row form never shows, so the row is a click target even when
    /// every piece fits.
    pub(crate) more: bool,
}

/// `s` with its first letter upper-cased — the daemon names an operation mid-sentence
/// (`re-seating: rendering …`) and the row starts with it.
fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// `done` of `total`, the numerator padded to the denominator's width so the clock after it does
/// not walk as the count grows (the operator: *"move cat to the right most position or thngs jump
/// around"* — the same rule, one widget later).
fn of(done: u64, total: u64) -> String {
    let t = thousands(total);
    format!("{:>n$}/{t}", thousands(done), n = t.chars().count())
}

impl Progress {
    /// **The row forms, fullest first**: everything; without the advice; without the clock;
    /// without the counts; the bar (with its percentage) alone; the label alone. A form identical
    /// to the one before it (no bar to drop) is skipped. `more` is the `+N` of other progresses
    /// live at the same time, carried with the label so it is never dropped either.
    pub(crate) fn forms(&self, more: usize, now_ms: u64) -> Vec<Line> {
        let mut label = vec![Span::role(self.label.clone(), self.role)];
        if more > 0 {
            label.push(Span::role(format!(" +{more}"), Role::Faint));
        }
        let bar_spans = self.bar.map(|p| {
            let mut s = bar(&p, PROGRESS_BAR_COLS);
            let pct = (p.fraction() * 100.0).floor() as u64;
            s.push(Span::role(format!(" {pct:>3}%"), Role::Faint));
            s
        });
        let clock = duration(now_ms.saturating_sub(self.since_ms));
        let build = |with_bar: bool, with_counts: bool, with_clock: bool, with_advice: bool| {
            let mut l = Line::new(label.clone());
            if with_bar && let Some(b) = &bar_spans {
                l.push(Span::raw(" · "));
                l.spans.extend(b.iter().cloned());
            }
            if with_counts && let Some(c) = &self.counts {
                l.push(Span::raw(" · "));
                l.push(Span::role(c.clone(), Role::Faint));
            }
            if with_clock {
                l.push(Span::raw(" · "));
                l.push(Span::role(clock.clone(), Role::Faint));
            }
            if with_advice && let Some(a) = &self.advice {
                l.push(Span::raw(" · "));
                l.push(Span::role(a.clone(), Role::Faint));
            }
            l
        };
        let mut out: Vec<Line> = Vec::new();
        for (b, c, k, a) in [
            (true, true, true, true),
            (true, true, true, false),
            (true, true, false, false),
            (true, false, false, false),
            (false, false, false, false),
        ] {
            let l = build(b, c, k, a);
            if out.last().is_none_or(|p| p.plain() != l.plain()) {
                out.push(l);
            }
        }
        out
    }
}

impl App {
    /// **Every progress live now, oldest first.** The status row shows the last of them — the one
    /// that started last, which is the one the reader just caused or just saw begin — and counts
    /// the rest as `+N` ([`App::status_row`]).
    ///
    /// * **A fill the daemon named** (`SessionEvent::Filling`): a reseat's render and write, a
    ///   compaction's carry, a restore, an import. Gated by [`MIN_FILLING`] as the block was — a
    ///   few rows are not worth a line that comes and goes in the status row either.
    /// * **A fold** (`SessionEvent::CompactionProgress`): reading its half, then writing.
    /// * **A snapshot's announced rows** ([`Bulk`]) while no fill is named — arriving, and past
    ///   [`BODY_PATIENCE`] the finding that they never did, with the ids.
    pub(crate) fn progresses(&self) -> Vec<Progress> {
        let now_ms = self.now_ms;
        let mut out: Vec<Progress> = Vec::new();
        if let Some((what, unit, done, total)) = &self.filling
            && *total >= MIN_FILLING
        {
            let counts = format!("{} {unit}", of(*done, *total));
            out.push(Progress {
                label: capital(what),
                role: Role::Pending,
                bar: Some(Prefill {
                    total: *total,
                    // **Settled, not computed**: a landed row is `█`, never the prefill's `▓`
                    // being-worked-on glyph — nothing here is being computed.
                    cache: *done,
                    processed: *done,
                    time_ms: 0,
                }),
                detail: format!("{what}\n\n{counts}"),
                counts: Some(counts),
                advice: None,
                since_ms: self.filling_since,
                more: false,
            });
        }
        if let Some(c) = &self.compacting {
            let label = format!("Compacting half {} of {}", c.half, c.halves);
            // **Reading, then writing** — the two phases of a fold, minutes apart. While the half's
            // prompt is read the bar measures it; once the model writes, the count of what it has
            // written is the only number there is.
            let (bar, counts) = if c.processed > 0 && c.processed < c.prompt_tokens {
                (
                    Some(Prefill {
                        total: c.prompt_tokens,
                        cache: 0,
                        processed: c.processed,
                        time_ms: 0,
                    }),
                    format!("{} tok read", of(c.processed, c.prompt_tokens)),
                )
            } else {
                (None, format!("{} {} written", thousands(c.written), c.unit))
            };
            out.push(Progress {
                detail: format!(
                    "compacting half {} of {} — {counts}\n\nNothing shows on the transcript until \
                     it lands, and the conversation is kept either way.",
                    c.half, c.halves
                ),
                label,
                role: Role::Pending,
                bar,
                counts: Some(counts),
                advice: None,
                since_ms: self.compacting_since,
                more: true,
            });
        }
        if self.filling.is_none()
            && let Some(b) = &self.bulk
        {
            let missing = b.ids.len();
            let total = b.announced.max(missing) as u64;
            let arrived = total - missing as u64;
            if now_ms.saturating_sub(b.at_ms) >= BODY_PATIENCE {
                // **What this head OBSERVED, and no cause it cannot see** — it was told these rows
                // exist and the bodies never arrived. The ids first, because the row is cut from
                // the right; all of them in the click and on `/status`.
                let mut ids: Vec<&str> = b.ids.iter().map(String::as_str).collect();
                ids.sort_unstable();
                let named = if missing <= 4 {
                    ids.join(", ")
                } else {
                    format!("{}, … ({} more)", ids[..3].join(", "), missing - 3)
                };
                out.push(Progress {
                    label: format!("{missing} row(s) never filled"),
                    role: Role::Attention,
                    bar: None,
                    counts: Some(named),
                    advice: Some("/resync clears this".to_string()),
                    since_ms: b.at_ms,
                    detail: format!(
                        "{missing} row(s) announced to this head and never filled: {} — \
                         `/resync` clears this",
                        ids.join(", ")
                    ),
                    more: missing > 4,
                });
            } else {
                let counts = format!("{} rows", of(arrived, total));
                out.push(Progress {
                    label: "Rows arriving".to_string(),
                    role: Role::Pending,
                    bar: Some(Prefill {
                        total,
                        cache: arrived,
                        processed: arrived,
                        time_ms: 0,
                    }),
                    detail: format!(
                        "{total} row(s) announced, waiting for the daemon to send them — \
                         {arrived} arrived, {missing} to come"
                    ),
                    counts: Some(counts),
                    advice: None,
                    since_ms: b.at_ms,
                    more: false,
                });
            }
        }
        // Stable: a tie keeps the order above.
        out.sort_by_key(|p| p.since_ms);
        out
    }

    /// **The progress the status row shows, and what a click on it would open** — `None` when
    /// nothing is in progress. The last one to start, its forms fullest first, and the detail of
    /// every one that is live.
    pub(crate) fn progress_shown(&self) -> Option<(Vec<Line>, String, bool)> {
        let all = self.progresses();
        let last = all.last()?;
        let forms = last.forms(all.len() - 1, self.now_ms);
        let detail = all
            .iter()
            .rev()
            .map(|p| p.detail.clone())
            .collect::<Vec<_>>()
            .join("\n\n———\n\n");
        let more = all.len() > 1 || last.more;
        Some((forms, detail, more))
    }
}

/// `line`, followed by `form` — joined by ` · ` after the turn's words, or, alone on an idle
/// row, behind a spinner of its own — and cut to `w` if the label itself does not fit. `None`
/// when not even [`super::turn_status::TRANSIENT_MIN`] columns of it would; the flag says it was
/// cut.
pub(crate) fn with_progress(
    line: &Line,
    form: &Line,
    w: usize,
    now_ms: u64,
) -> Option<(Line, bool)> {
    let mut out = line.clone();
    if out.width() == 0 {
        out.push(Span::role(
            format!("{} ", rano::agent::text::spinner(now_ms)),
            Role::Pending,
        ));
    } else {
        out.push(Span::raw(" · "));
    }
    let room = w.saturating_sub(out.width());
    if form.width() <= room {
        out.spans.extend(form.spans.iter().cloned());
        return Some((out, false));
    }
    if room < super::turn_status::TRANSIENT_MIN {
        return None;
    }
    // The label alone, cut: still the thing it is about.
    let cut = trim_to(&form.plain(), room);
    debug_assert!(visible_width(&cut) <= room);
    let style = form
        .spans
        .first()
        .map(|s| s.style.clone())
        .unwrap_or_default();
    out.push(Span {
        content: cut,
        style,
    });
    Some((out, true))
}
