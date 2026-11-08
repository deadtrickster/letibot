//! A background job's completion notice, drawn as two lines a job - and its whole card one
//! click away.
//!
//! The row is the daemon's `completion_notice` (`crates/harnessd/src/harness.rs`), a `[job] …` user row, and the model's text is untouched.
//! This renders it as [`folded_notice`] does, beside that fold because a multi-line command and the `memory:` line are text the fold
//! does not recognise, so it handed the whole row back raw.
//!
//! # The two lines
//!
//! ```text
//! j1042 · build-and-test-null · exited 0 2m54s · bash -c 'cd /home/dead/Projects/dbtest/pg-n…
//!   ⎿ test result: ok. 12 passed; 0 failed
//! ```
//!
//! The first line is the job: id, name, how it ended, how long, and the command flattened and cut to the width. The second is the last line
//! the job wrote, since a build says how it went at its end. The head reads the job's tail like the jobs pane (`ReadJobOutput`), once per
//! job and only for a notice on screen; until it lands, or when the output is unreadable, the line says what the notice knows.
//!
//! [`folded_notice`]'s rule, kept: every line of the notice is accounted for - the heading is one of the four `completion_notice` writes, the
//! bullet count matches, every bullet parses, and the closing is one of the daemon's two. Anything else is `None` and the row is drawn as
//! before. The parser is pinned against the daemon's own output (`app/tests/fixtures/job_notice_*.txt`, compared byte for byte).

use std::collections::HashMap;

use crate::app::SubagentState;
use crate::ui::render::trim_to;
use crate::ui::*;

/// The closing paragraphs `completion_notice` ends a `[job]` group with, by their opening
/// words: the owner's and the one a parent is told when the session
/// that started the job has stopped.
const CLOSINGS: &[&str] = &[
    // `harnessd`'s `JOB_CLOSING`.
    "Output: `job_output` ",
    // The older closing, still in stored transcripts.
    "This is the completion arriving on its own — you do not need to wait for it, and `job_wait`",
    "This session is told because the one that started it cannot be:",
    // The later closing, which names the starter that has stopped instead.
    "(Started by ",
];

/// The closings only a settlement handed up carries - the older paragraph and the
/// clause after it - so which heading a group belongs under is decided by the set, not by order.
const HANDED_UP: &[&str] = &[
    "This session is told because the one that started it cannot be:",
    "(Started by ",
];

/// The headings, as `(singular, plural suffix)`: `a job you backgrounded has ended:` and
/// `N jobs you backgrounded have ended:`, and the same two for a job handed up.
const HEADINGS: &[(&str, &str, bool)] = &[
    (
        "a job you backgrounded has ended:",
        " jobs you backgrounded have ended:",
        true,
    ),
    (
        "a job a session below this one started has ended:",
        " jobs a session below this one started have ended:",
        false,
    ),
];

/// One job a notice reports ended, as the bullet says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndedJob {
    /// The handle, `j1042` - what `ReadJobOutput` takes.
    pub(crate) id: String,
    /// The name the agent gave it, when it gave one.
    pub(crate) slug: Option<String>,
    /// The process's own word - `exited 0`, `killed by job_kill`.
    pub(crate) state: String,
    /// How long it ran, in the daemon's spelling (`2m54s`, `4.4s`).
    pub(crate) after: String,
    /// Bytes it wrote, all streams - `None` for a newer notice, which drops the
    /// count from the model's sentence.
    pub(crate) produced: Option<u64>,
    /// The command, verbatim - several lines when it was written over several.
    pub(crate) command: String,
    /// `memory: …`'s clause, when the notice carried one.
    pub(crate) memory: Option<String>,
    /// The session that started it, when it is not this one (the handed-up notice).
    pub(crate) owner: Option<String>,
    /// The notice said the output is partial: a kill ends the work, not the output, and a card
    /// that drew only `killed by …` would read as a finish.
    pub(crate) partial: bool,
    /// The files that changed in the job's tree while it ran, as the daemon's line names them - its
    /// own line, because a job says WHEN and not WHO (R161).
    pub(crate) changed: Option<String>,
}

/// The opening words of the line a settlement carries about the job's tree, written by
/// `harnessd`'s `changed_sentence`.
pub(crate) const CHANGED: &str = "changed while it ran: ";

/// A `[job]` notice group, parsed - whether it is this session's own (`mine`) and the jobs
/// it reports. `None` for anything that is not exactly the shape `completion_notice` writes.
pub(crate) fn ended_jobs(group: &str) -> Option<(bool, Vec<EndedJob>)> {
    let rest = group.strip_prefix("[job] ")?;
    let (head, rest) = rest.split_once('\n')?;
    let (mine, want) = HEADINGS.iter().find_map(|(one, many, mine)| {
        if head == *one {
            return Some((*mine, 1usize));
        }
        let n = head.strip_suffix(many)?.parse::<usize>().ok()?;
        (n > 1).then_some((*mine, n))
    })?;
    // The closing paragraph is the group's end, found by its words rather than by a blank
    // line: a command written over several lines can hold a blank line of its own.
    let (body, closing) = CLOSINGS
        .iter()
        .filter_map(|c| rest.find(&format!("\n{c}")).map(|at| (at, *c)))
        .min_by_key(|(at, _)| *at)
        .map(|(at, _)| (&rest[..at], &rest[at + 1..]))
        .or_else(|| {
            // The closing directly under the heading would be a group with no bullets, which is
            // refused below anyway; this keeps the slicing honest for it.
            CLOSINGS
                .iter()
                .any(|c| rest.starts_with(c))
                .then_some(("", rest))
        })?;
    // The closing is one paragraph, followed at most by the line naming the plan rows that
    // wait on these jobs ([`waiting_line`]); any other line after it is something this head was
    // not told about.
    let closing = match closing.split_once('\n') {
        None => closing,
        Some((closing, after)) if is_waiting_line(after) => closing,
        Some(_) => return None,
    };
    // Which closing goes with which heading is the daemon's rule, and a mismatch is a shape this
    // head does not know: a `HANDED_UP` closing belongs to the handed-up heading and no other.
    if !HANDED_UP.iter().any(|c| closing.starts_with(c)) != mine {
        return None;
    }
    let mut jobs: Vec<EndedJob> = Vec::new();
    for line in body.split('\n') {
        if let Some(bullet) = line.strip_prefix("  - `") {
            jobs.push(bullet_of(bullet)?);
        } else if let Some(note) = line.strip_prefix("    ") {
            let job = jobs.last_mut()?;
            // The partial sentence is the LAST line of a job's block, written by
            // `completion_notice` after the memory clause: nothing may follow it for that job.
            if job.partial {
                return None;
            }
            if note.starts_with("its output is partial") {
                job.partial = true;
            } else if let Some(m) = note.strip_prefix("memory: ") {
                if job.memory.is_some() {
                    return None;
                }
                job.memory = Some(m.to_string());
            } else if let Some(m) = note.strip_prefix(CHANGED) {
                // What the job's tree looked like when it ended (R161): its own line, never the
                // command's continuation - a diff card would otherwise be the last half of a shell
                // line nobody ran.
                if job.changed.is_some() {
                    return None;
                }
                job.changed = Some(m.to_string());
            } else {
                // A command's own next line - only before the memory clause, which the daemon
                // writes after the bullet's last line.
                if job.memory.is_some() {
                    return None;
                }
                job.command.push('\n');
                job.command.push_str(line);
            }
        } else {
            // A command's own next line - only before the memory clause, which the daemon
            // writes after the bullet's last line.
            let job = jobs.last_mut()?;
            if job.memory.is_some() || job.partial {
                return None;
            }
            job.command.push('\n');
            job.command.push_str(line);
        }
    }
    if jobs.len() != want {
        return None;
    }
    // Whose it was, on the end of the command's last line, for a handed-up notice only.
    if !mine {
        for job in &mut jobs {
            let at = job.command.rfind(" (started by `")?;
            let owner = job.command[at..]
                .strip_prefix(" (started by `")?
                .strip_suffix("`)")?;
            job.owner = Some(owner.to_string());
            job.command.truncate(at);
        }
    }
    Some((mine, jobs))
}

/// The opening words of the daemon's line naming the plan rows that wait on a completion  -
/// `harnessd`'s `WAITING_ON_THIS` / `WAITING_ON_THESE`, which it writes as the last line of a
/// `[job]` or `[task]` group, after the closing paragraph.
pub(crate) const WAITING: &[&str] = &["Waiting on this: ", "Waiting on these: "];

/// Whether `line` is exactly one such line.
pub(crate) fn is_waiting_line(line: &str) -> bool {
    !line.contains('\n') && WAITING.iter().any(|w| line.starts_with(w))
}

/// The waiting line of a notice group, folded for a person: the rows it names, without the
/// instruction to the model that follows the dash - `Waiting on this: "make tier 1 green" (the
/// operator's)`. `None` when the group has no such line.
pub(crate) fn waiting_line(group: &str) -> Option<String> {
    let line = group.lines().rev().find(|l| is_waiting_line(l))?;
    Some(folded_waiting(line))
}

/// One waiting line, cut before ` - update …`, which is addressed to the model.
pub(crate) fn folded_waiting(line: &str) -> String {
    match line.rfind(" — update ") {
        Some(at) => line[..at].to_string(),
        None => line.to_string(),
    }
}

/// The first line of a bullet, after the backtick, in either shape this tree writes: `LABEL` STATE
/// after DUR, wrote N bytes: CMD, or `LABEL` STATE after DUR[: CMD]. A duration never holds `: `,
/// so the first `: ` after the last ` after ` before it is where the command starts.
fn bullet_of(bullet: &str) -> Option<EndedJob> {
    let (label, said) = bullet.split_once('`')?;
    let said = said.strip_prefix(' ')?;
    let (state, after, produced, command) = match said.split_once(", wrote ") {
        Some((ended, wrote)) => {
            let (state, after) = ended.rsplit_once(" after ")?;
            let (bytes, command) = wrote.split_once(" bytes: ")?;
            (state, after, Some(bytes.parse::<u64>().ok()?), command)
        }
        None => {
            let (ended, command) = said.split_once(": ").unwrap_or((said, ""));
            let (state, after) = ended.rsplit_once(" after ")?;
            (state, after, None, command)
        }
    };
    // The label is `job_label`'s: `slug · j65`, or the bare id when nobody named the job.
    let (slug, id) = match label.rsplit_once(" · ") {
        Some((slug, id)) if is_job_id(id) => (Some(slug.to_string()), id.to_string()),
        _ => (None, label.to_string()),
    };
    if id.is_empty() || state.is_empty() {
        return None;
    }
    Some(EndedJob {
        id,
        slug,
        state: state.to_string(),
        after: after.to_string(),
        produced,
        command: command.to_string(),
        memory: None,
        owner: None,
        partial: false,
        changed: None,
    })
}

fn is_job_id(s: &str) -> bool {
    s.strip_prefix('j')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What this head holds of a job's output, for the notice's second line and its popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobTail {
    /// Asked for; the answer has not landed.
    Asked,
    /// The last lines the job wrote - a window that reached the end of its output.
    Lines(Vec<String>),
    /// The daemon cannot show it (the job was reaped and only its record is left, or the read
    /// was refused); the reason, in its words.
    Gone(String),
}

/// How much of a job's end the head asks for: enough for the popup to show a screen of it.
pub(crate) const TAIL_BYTES: u64 = 4096;

/// The groups of a notice row, split where a new labelled notice starts. The daemon joins the
/// notices of one wake with a blank line (`completion_notices`), and a `[job]` group ends at its
/// own closing paragraph - so a blank line inside a command does not split it.
pub(crate) fn notice_groups(text: &str) -> Vec<&str> {
    // `[meanwhile] ` is the line a held batch opens with; the groups under it are the
    // ordinary `[job]` / `[task]` ones (`job_notice_meanwhile.txt`).
    const TAGS: &[&str] = &[
        "[job] ",
        "[task] ",
        "[todo check] ",
        "[monitor] ",
        "[meanwhile] ",
    ];
    let mut out = Vec::new();
    let mut start = 0;
    let mut from = 0;
    while let Some(at) = text[from..].find("\n\n").map(|i| from + i) {
        let next = &text[at + 2..];
        // A `[job]` group's command may hold a blank line followed by anything, so a split
        // inside one waits for its closing paragraph to have been seen.
        let closed = !text[start..].starts_with("[job] ")
            || CLOSINGS.iter().any(|c| text[start..at].contains(c));
        if closed && TAGS.iter().any(|t| next.starts_with(t)) {
            out.push(&text[start..at]);
            start = at + 2;
        }
        from = at + 2;
    }
    out.push(&text[start..]);
    out
}

/// The jobs a notice row reports, across every `[job]` group in it - `(mine, job)` - or
/// `None` when the row has no `[job]` group this head can read in full.
pub(crate) fn notice_jobs(text: &str) -> Option<Vec<(bool, EndedJob)>> {
    if !text.starts_with('[') {
        return None;
    }
    let mut out = Vec::new();
    for g in notice_groups(text) {
        if g.starts_with("[job] ") {
            let (mine, jobs) = ended_jobs(g)?;
            out.extend(jobs.into_iter().map(|j| (mine, j)));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The compact row: two lines per ended job, every other group of the row folded the way
/// [`folded_notice`] folds it. `None` when any group is a shape this head does not know, so the row
/// is then drawn as the fold or as it arrived. Lines are cut to `cols` with the head's own ellipsis.
pub(crate) fn compact_notice(
    text: &str,
    subagents: &[SubagentState],
    tails: &HashMap<String, JobTail>,
    cols: usize,
) -> Option<Vec<String>> {
    if !text.starts_with("[job] ") && !text.contains("\n\n[job] ") {
        return None;
    }
    let mut out = Vec::new();
    for g in notice_groups(text) {
        if g.starts_with("[job] ") {
            let (mine, jobs) = ended_jobs(g)?;
            for j in &jobs {
                let [a, b] = job_lines(j, mine, tails.get(&j.id));
                out.push(trim_to(&a, cols));
                out.push(trim_to(&b, cols));
            }
            if let Some(w) = waiting_line(g) {
                out.push(trim_to(&w, cols));
            }
        } else {
            for l in folded_notice(g, subagents)?.lines() {
                out.push(trim_to(l, cols));
            }
        }
    }
    Some(out)
}

/// One job's two lines, uncut.
pub(crate) fn job_lines(j: &EndedJob, mine: bool, tail: Option<&JobTail>) -> [String; 2] {
    let mut first = j.id.clone();
    if let Some(s) = &j.slug {
        first.push_str(" · ");
        first.push_str(s);
    }
    first.push_str(&format!(" · {} {}", j.state, j.after));
    if let Some(o) = &j.owner {
        first.push_str(&format!(" · started by {o}"));
    }
    if !j.command.is_empty() {
        first.push_str(" · ");
        first.push_str(&flat(&j.command));
    }
    let said = match tail {
        Some(JobTail::Lines(lines)) => lines
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| flat(l)),
        _ => None,
    };
    let second = match said {
        Some(l) => l,
        None => {
            let mut facts = match (j.produced, j.partial) {
                (Some(0), false) => "wrote nothing".to_string(),
                (Some(n), false) => format!("wrote {n} bytes"),
                (Some(n), true) => format!("wrote {n} bytes so far · output is partial"),
                (None, true) => "ended early · output is partial".to_string(),
                // The notice no longer counts; the tail, when fetched, replaces this.
                (None, false) => "ended".to_string(),
            };
            if let Some(m) = &j.memory {
                facts.push_str(&format!(" · memory: {m}"));
            }
            if !mine {
                // The notice's own word for it: the output went with the session that stopped.
                facts.push_str(" · output not readable here");
            }
            facts
        }
    };
    // What changed in the job's tree while it ran, on the line that carries the other facts: the
    // daemon measured it (R161), and the row is where its diff is drawn.
    let second = match &j.changed {
        Some(c) => format!("{second} · changed while it ran: {c}"),
        None => second,
    };
    [first, format!("  ⎿ {second}")]
}

/// Whitespace, newlines included, collapsed to single spaces - a command written over ten lines
/// is one line of a row.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
