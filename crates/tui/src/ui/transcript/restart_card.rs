//! A daemon restart drawn as ONE line, with what the daemon said one click away in the popup the
//! job card opens. The shapes it folds:
//!
//! ```text
//! restarted, stopped by operator · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 job · click for details
//! crashed, down ~3h12m · reminded: 1 subagent, 1 monitor, 1 job · click for details
//! crashed, stop time unknown · reminded: 1 job · click for details
//! restarted, stopped by operator · down 1s · click for details
//! session · restarted, stopped by operator · down 4m12s · restored: 2 subagents, 1 monitor · reminded: 1 job · click for details
//! session · restarted · carrying on: your promise, 2 open todo rows · click for details
//! ```
//!
//! The counts come from the hub warning or the session notice, whichever opened the row; an unrecognised line is `None` and the popup keeps the text.
//!
//! The register follows what the store knows: an orderly `letibot --stop` records its own stop time,
//! so a down time that is not an estimate is the operator's own act and draws as news. A down time
//! that IS an estimate, or no record at all, is a daemon that went down unwatched, and that is the
//! one a person has to look at.

use letibot_sessionlog::pane::Role as Meaning;

use crate::ui::transcript::line_cards::{CLICK, Card, SessionKind, WarningKind};

/// What every restart notice opens with.
const OPENS: &str = "[restart] the daemon ";
/// The opening a newer notice carries: what happened, then the clock in brackets.
const RESTARTED: &str = "[restart] The daemon restarted";
/// The empty restart's own end, which is a card with no list beneath it.
const NOTHING_RUNNING: &str = "Nothing was running.";
/// A known down time: `… was down for 4m12s (stopped at …; back at …) - …`.
const DOWN_FOR: &str = "[restart] the daemon was down for ";
/// A restart with nothing to restore: `… came back at 21:28:20 (down 1s, stopped at …):
/// nothing was running`. Its return time leads it, so the down time is a clause rather than the
/// opening - see `restart::nothing_running`.
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

/// One restart, as the daemon's text says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Restart {
    pub(crate) down: Down,
    pub(crate) restored: Counts,
    pub(crate) reminded: Counts,
    pub(crate) not_restored: Counts,
    /// What a restart that woke the session asks of it, short: `your promise, 2 open todo rows`.
    pub(crate) owed: Option<String>,
}

impl Restart {
    /// Whether the store has this daemon's own stop time - an orderly `letibot --stop` - rather
    /// than an estimate from a heartbeat, or no record at all.
    fn orderly(&self) -> bool {
        match self.down {
            Down::Known { estimated, .. } => !estimated,
            Down::Unknown => false,
        }
    }

    /// `restarted, stopped by operator · down 4m12s` - or the crash's own two words. A restart
    /// that owes work leads with the work: the operator knows they restarted it.
    fn head(&self) -> String {
        if let Some(owed) = &self.owed {
            return match self.orderly() {
                true => format!("restarted · carrying on: {owed}"),
                false => format!("{} · carrying on: {owed}", self.bare_head()),
            };
        }
        self.bare_head()
    }

    fn bare_head(&self) -> String {
        match &self.down {
            Down::Known { span, .. } if self.orderly() => {
                format!("restarted, stopped by operator · down {span}")
            }
            Down::Known { span, .. } => format!("crashed, down ~{span}"),
            Down::Unknown => "crashed, stop time unknown".into(),
        }
    }

    /// The whole line, uncut.
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
        // The verdict is this notice's, not the code's: `restart_restored` is one code for the
        // operator's own restart and for a crash, and only the second is worth a person's eyes.
        // The colour still comes from the head's one meaning-to-role mapping.
        let (meaning, word) = if self.orderly() {
            (Meaning::Info, "restarted")
        } else {
            (Meaning::Attention, "crashed")
        };
        Card::new(
            self.line(),
            0,
            word,
            crate::ui::severity::role(Some(meaning)),
            self.head(),
        )
    }
}

/// The header line, read - `None` for an opening this head does not know.
fn down_of(header: &str) -> Option<Down> {
    // The shape every notice carries now: `(down 4m12s)`, `(down ~3h12m)` or
    // `(stop time unknown)`.
    if let Some(r) = header.strip_prefix(RESTARTED) {
        let inside = r.strip_prefix(" (")?.split(')').next()?;
        let Some(span) = inside.strip_prefix("down ") else {
            return inside
                .starts_with("stop time unknown")
                .then_some(Down::Unknown);
        };
        let (estimated, span) = match span.strip_prefix('~') {
            Some(s) => (true, s),
            None => (false, span),
        };
        if span.is_empty() || span.contains(' ') || !span.ends_with(['s', 'm']) {
            return None;
        }
        return Some(Down::Known {
            span: span.to_string(),
            estimated,
        });
    }
    // The empty restart, whose down time is a clause after the return time: `(down 1s,
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
        owed: None,
    }
}

/// What `restart::Owed::clause` and `nothing_running_but` said, short - `None` for a notice that
/// asks nothing. Read off the daemon's fixed words; a todo row's own words are never matched.
fn owed_of(text: &str) -> Option<String> {
    let rest = text.split_once(NOTHING_RUNNING)?.1;
    if rest.starts_with(" This turn is what you told the operator you would do once they restarted")
    {
        return Some("your promise".into());
    }
    let rest = rest.strip_prefix(" This session owes work — ")?;
    let (work, tail) = rest.split_once(". Carry on with that work now")?;
    let mut said: Vec<String> = Vec::new();
    let mut work = work;
    if let Some(r) = work.strip_prefix("a turn was in flight when the rows stopped") {
        said.push("a turn cut off".into());
        work = r.strip_prefix(", ").unwrap_or(r);
    }
    let count = |work: &str, one: &str, many: &str| -> Option<usize> {
        if work.starts_with(&format!("1 {one}")) {
            return Some(1);
        }
        let (n, r) = work.split_once(' ')?;
        r.starts_with(many).then(|| n.parse().ok()).flatten()
    };
    let n_of = |n: usize, word: &str| match n {
        1 => format!("1 {word}"),
        n => format!("{n} {word}s"),
    };
    if let Some(n) = count(
        work,
        "a settlement nobody has been told about",
        "settlements nobody has been told about",
    ) {
        said.push(n_of(n, "undelivered result"));
        work = work.split_once(", ").map_or("", |(_, r)| r);
    }
    if let Some(n) = count(
        work,
        "a todo row whose condition is met",
        "todo rows whose condition is met",
    ) {
        said.push(n_of(n, "due todo row"));
        // The row contents follow, so the next clause is found by its own fixed words.
        work = work
            .find(" todo row still owed")
            .or_else(|| work.find(" todo rows still owed"))
            .map_or("", |i| {
                let start = work[..i].rfind(", ").map_or(0, |c| c + 2);
                &work[start..]
            });
    }
    if let Some(n) = count(work, "a todo row still owed", "todo rows still owed") {
        said.push(n_of(n, "open todo row"));
    }
    if tail.starts_with(", including what you told the operator you would do") {
        said.push("your promise".into());
    }
    (!said.is_empty()).then(|| said.join(", "))
}

/// The operator's own restart that asked nothing of the session: the one sentence and no
/// list. Drawn only at `loud` - the operator knows they restarted it.
pub(crate) fn quiet_operator_restart(text: &str) -> bool {
    let t = text.trim();
    if t.lines().count() != 1 {
        return false;
    }
    let Some(r) = notice_restart(t) else {
        return false;
    };
    let empty = r.restored == Counts::default()
        && r.reminded == Counts::default()
        && r.not_restored == Counts::default();
    if !(r.orderly() && empty && r.owed.is_none()) {
        return false;
    }
    if let Some((_, tail)) = t.split_once(NOTHING_RUNNING) {
        return tail.is_empty()
            || tail
                .strip_prefix(" The stop is older than the ")
                .is_some_and(|w| w.ends_with(" restore window.") && !w.contains(". "));
    }
    t.starts_with(CAME_BACK) && t.trim_end_matches('.').ends_with(": nothing was running")
}

/// A `restart_restored` / `restart_reminded` / `restart_nothing` warning's detail, read.
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
    // A header with nothing under it is a card only for the empty restart, which has no list
    // to have: every other opening is a list that came back empty, and a shape this head cannot
    // count is left to the popup rather than drawn as *"restarted"* and nothing else.
    (any || head.starts_with(CAME_BACK) || head.contains(NOTHING_RUNNING)).then_some(r)
}

/// A session's `[restart] …` notice, read - the model's words for the same counts.
pub(crate) fn notice_restart(text: &str) -> Option<Restart> {
    let mut lines = text.lines();
    let mut r = empty(down_of(lines.next()?)?);
    r.owed = owed_of(text);
    // Inside a `[task]` group the restart carries (`SESSION.carries`): the children it handed
    // over, one bullet each - what the warning says as `had finished … delivering it`.
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
        if let Some(s) = l.strip_prefix("- subagent ") {
            // A bullet, not a second `[restart]` notice inside this one. The old
            // spellings stay readable for every stored transcript.
            if s.contains(": resumed") || s.contains("has been resumed") {
                r.restored.add(Kind::Subagent);
            } else if s.contains(": not resumed") || s.contains("could NOT be") {
                r.not_restored.add(Kind::Subagent);
            } else if s.contains("was NOT resumed") {
                r.reminded.add(Kind::Subagent);
            }
        } else if let Some(s) = l.strip_prefix("[restart] subagent ") {
            if s.contains("has been resumed") {
                r.restored.add(Kind::Subagent);
            } else if s.contains("could NOT be") {
                r.not_restored.add(Kind::Subagent);
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

/// The restart notice in the card registry (`line_cards`). Its lines are joined by single newlines,
/// so it ends at the first full stop or colon a blank line and another notice follow - except the
/// `[task]` groups it carries, whose children are counted on the line, not drawn as a fold under it.
pub(crate) const SESSION: SessionKind = SessionKind {
    opens: opens_notice,
    ends: &[".", ":"],
    read: notice_card,
    carries: &["[task] "],
};

/// And its hub warnings. Its register is the notice's, not the code's: two of the three codes are
/// raised for an orderly stop and for a crash alike.
pub(crate) const WARNING: WarningKind = WarningKind {
    codes: &["restart_restored", "restart_reminded", "restart_nothing"],
    read: warning_card,
    from_code: false,
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
