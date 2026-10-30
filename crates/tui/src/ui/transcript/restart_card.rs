//! **A daemon restart, drawn as ONE line** — and the whole of what the daemon said one click
//! away, in the popup the job card opens.
//!
//! The operator, pasting what pg-noop's head drew after its daemon came back on a new build — a
//! `restart_restored` note that wrapped into a paragraph, and the session's `[restart] …` notice
//! to the model under it with the job's whole command and its explanation: *"these gray walls
//! are hilarious … make them the usual one liners"*.
//!
//! # The lines
//!
//! ```text
//! restarted · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 job · click for details
//! restarted · down ~3h12m · reminded: 1 subagent, 1 monitor, 1 job · click for details
//! restarted · stop time unknown · reminded: 1 job · click for details
//! restarted · down 1s · click for details
//! session · restarted · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 job · click for details
//! ```
//!
//! **The fourth is a restart with nothing to restore** (`restart_nothing`): the daemon came back
//! and says when, and there is no list under it. The operator, 2026-10-30: *"mb always tell agent
//! about restart? with the current time lol"* — so that sentence is its own opening
//! (`[restart] the daemon came back at … (down 1s, stopped at …): nothing was running`) and the
//! line it draws has no counts in it.
//!
//! The hub warning (`restart_restored` / `restart_reminded`, `restart::warning_detail` in
//! harnessd) is a header line and one `  - KIND NAME: VERDICT` line per thing; the session notice
//! (`restart::notice`) is the same header and one line per thing in the model's words. Both are
//! read for the same counts: what was brought back (a subagent resumed or delivered, a monitor
//! re-armed), what was only reminded (every job, and everything past the threshold), and what
//! could NOT be brought back. `~` marks a down time estimated from a heartbeat; a stop no record
//! dates is said as unknown — and so is the older daemons' *"down for 0s … estimated from
//! nothing"*, which meant the same and said it wrong.
//!
//! # Only what this head can read is folded
//!
//! The header must be one of the daemon's two openings with a duration (or the unknown stop) in
//! it; anything else is `None` and the row is drawn as before. A line the reader does not
//! recognise is left to the popup — the line is a summary, and the popup is the text verbatim.

use rano::style::Role;

use crate::ui::transcript::line_cards::{CLICK, Card, SessionKind, WarningKind};

/// What every restart notice opens with.
const OPENS: &str = "[restart] the daemon ";
/// A known down time: `… was down for 4m12s (stopped at …; back at …) — …`.
const DOWN_FOR: &str = "[restart] the daemon was down for ";
/// **A restart with nothing to restore**: `… came back at 21:28:20 (down 1s, stopped at …):
/// nothing was running`. Its return time leads it, so the down time is a clause rather than the
/// opening — see `restart::nothing_running`.
const CAME_BACK: &str = "[restart] the daemon came back at ";
/// An unknown one: `… restarted at …; when the previous one stopped is unknown …`.
const UNKNOWN: &str = "when the previous one stopped is unknown";
/// What the daemons before this build said for an unknown stop.
const OLD_UNKNOWN: &str = "estimated from nothing";

/// How long the daemon was down, as the header says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Down {
    /// `4m12s`, and whether it was estimated (from a heartbeat).
    Known { span: String, estimated: bool },
    /// No record says when the previous daemon stopped.
    Unknown,
}

/// How many of each kind, in one bucket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) subagents: usize,
    pub(crate) monitors: usize,
    pub(crate) jobs: usize,
}

impl Counts {
    fn add(&mut self, kind: Kind) {
        match kind {
            Kind::Subagent => self.subagents += 1,
            Kind::Monitor => self.monitors += 1,
            Kind::Job => self.jobs += 1,
        }
    }

    /// `2 subagents, 1 monitor`, or `None` for an empty bucket.
    fn said(&self) -> Option<String> {
        let n = |c: usize, one: &str| match c {
            0 => None,
            1 => Some(format!("1 {one}")),
            c => Some(format!("{c} {one}s")),
        };
        let parts: Vec<String> = [
            n(self.subagents, "subagent"),
            n(self.monitors, "monitor"),
            n(self.jobs, "job"),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Subagent,
    Monitor,
    Job,
}

/// **One restart, as the daemon's text says it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Restart {
    pub(crate) down: Down,
    pub(crate) restored: Counts,
    pub(crate) reminded: Counts,
    pub(crate) not_restored: Counts,
}

impl Restart {
    /// `restarted · down 4m12s`.
    fn head(&self) -> String {
        match &self.down {
            Down::Known { span, estimated } => {
                format!(
                    "restarted · down {}{span}",
                    if *estimated { "~" } else { "" }
                )
            }
            Down::Unknown => "restarted · stop time unknown".into(),
        }
    }

    /// **The whole line, uncut.**
    pub(crate) fn line(&self) -> String {
        let mut parts = vec![self.head()];
        if let Some(s) = self.restored.said() {
            parts.push(format!("restored: {s}"));
        }
        if let Some(s) = self.not_restored.said() {
            parts.push(format!("NOT restored: {s}"));
        }
        if let Some(s) = self.reminded.said() {
            parts.push(format!("reminded: {s}"));
        }
        parts.push(CLICK.into());
        parts.join(" · ")
    }

    fn card(&self) -> Card {
        Card::new(self.line(), 0, "restarted", Role::Attention, self.head())
    }
}

/// **The header line, read** — `None` for an opening this head does not know.
fn down_of(header: &str) -> Option<Down> {
    // **The empty restart**, whose down time is a clause after the return time: `(down 1s,
    // stopped at …)` or `(down unknown, and no record says …)`.
    if let Some(r) = header.strip_prefix(CAME_BACK) {
        let down = r.split_once("(down ")?.1;
        let span = down.split([',', ')']).next()?;
        if span == "unknown" {
            return Some(Down::Unknown);
        }
        if span.is_empty() || span.contains(' ') || !span.ends_with(['s', 'm']) {
            return None;
        }
        return Some(Down::Known {
            span: span.to_string(),
            estimated: down.contains(", estimated from "),
        });
    }
    if let Some(r) = header.strip_prefix(DOWN_FOR) {
        let (span, rest) = r.split_once(" (")?;
        if span.is_empty() || span.contains(' ') || !span.ends_with(['s', 'm']) {
            return None;
        }
        if rest.contains(OLD_UNKNOWN) {
            return Some(Down::Unknown);
        }
        return Some(Down::Known {
            span: span.to_string(),
            estimated: rest.contains(", estimated from "),
        });
    }
    (header.starts_with(OPENS) && header.contains(UNKNOWN)).then_some(Down::Unknown)
}

fn empty(down: Down) -> Restart {
    Restart {
        down,
        restored: Counts::default(),
        reminded: Counts::default(),
        not_restored: Counts::default(),
    }
}

/// **A `restart_restored` / `restart_reminded` / `restart_nothing` warning's detail, read.**
pub(crate) fn warning_restart(detail: &str) -> Option<Restart> {
    let mut lines = detail.lines();
    let head = lines.next()?;
    let mut r = empty(down_of(head)?);
    let mut any = false;
    for l in lines {
        let Some(said) = l.strip_prefix("  - ") else {
            continue;
        };
        let kind = if said.starts_with("subagent ") {
            Kind::Subagent
        } else if said.starts_with("monitor ") {
            Kind::Monitor
        } else if said.starts_with("job ") {
            Kind::Job
        } else {
            continue;
        };
        if said.contains(": NOT ") {
            r.not_restored.add(kind);
        } else if said.contains(": reminded") {
            r.reminded.add(kind);
        } else if said.ends_with(": resumed")
            || said.ends_with(": re-armed")
            || said.contains(": had finished")
        {
            r.restored.add(kind);
        } else {
            continue;
        }
        any = true;
    }
    // **A header with nothing under it is a card only for the empty restart**, which has no list
    // to have: every other opening is a list that came back empty, and a shape this head cannot
    // count is left to the popup rather than drawn as *"restarted"* and nothing else.
    (any || head.starts_with(CAME_BACK)).then_some(r)
}

/// **A session's `[restart] …` notice, read** — the model's words for the same counts.
pub(crate) fn notice_restart(text: &str) -> Option<Restart> {
    let mut lines = text.lines();
    let mut r = empty(down_of(lines.next()?)?);
    // **Inside a `[task]` group the restart carries** (`SESSION.carries`): the children it handed
    // over, one bullet each — what the warning says as `had finished … delivering it`.
    let mut handed_over = false;
    for l in lines {
        if l.starts_with("[task] ") {
            handed_over = true;
            continue;
        }
        if handed_over {
            if l.starts_with("  - `") {
                r.restored.add(Kind::Subagent);
            }
            continue;
        }
        if let Some(s) = l.strip_prefix("[restart] subagent ") {
            if s.contains("has been resumed") {
                r.restored.add(Kind::Subagent);
            } else if s.contains("could NOT be") {
                r.not_restored.add(Kind::Subagent);
            }
        } else if let Some(s) = l.strip_prefix("- subagent ") {
            if s.contains("was NOT resumed") {
                r.reminded.add(Kind::Subagent);
            }
        } else if let Some(s) = l.strip_prefix("- monitor `") {
            if s.contains("` was re-armed") {
                r.restored.add(Kind::Monitor);
            } else if s.contains("` could NOT be re-armed") {
                r.not_restored.add(Kind::Monitor);
            } else if s.contains("` was watching") {
                r.reminded.add(Kind::Monitor);
            }
        } else if l.starts_with("- job `") {
            r.reminded.add(Kind::Job);
        }
    }
    Some(r)
}

/// **The restart notice in the card registry** (`line_cards`). It is one paragraph (its lines
/// are joined by single newlines), so it ends at the first full stop or colon a blank line and
/// another notice follow — **except the `[task]` groups it carries**: the children a restart
/// handed over are counted on its line (`restored: 12 subagents`), not drawn as a fold under it
/// (pg-noop, 2026-10-14: twelve `Agent …` lines under a card that already said it).
pub(crate) const SESSION: SessionKind = SessionKind {
    opens: opens_notice,
    ends: &[".", ":"],
    read: notice_card,
    carries: &["[task] "],
};

/// **And its hub warnings.**
pub(crate) const WARNING: WarningKind = WarningKind {
    codes: &["restart_restored", "restart_reminded", "restart_nothing"],
    read: warning_card,
};

fn opens_notice(text: &str) -> bool {
    text.lines().next().and_then(down_of).is_some()
}

fn notice_card(text: &str) -> Option<Card> {
    notice_restart(text).map(|r| r.card())
}

fn warning_card(detail: &str) -> Option<Card> {
    warning_restart(detail).map(|r| r.card())
}
