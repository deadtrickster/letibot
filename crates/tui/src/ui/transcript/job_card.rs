//! **A background job's completion notice, drawn as two lines a job** — and its whole card one
//! click away.
//!
//! The operator, pasting a notice that took six wrapped rows of the transcript: *"i dont need job
//! completion card that huge - first line is j<id> - slug - truncated cmd and second line is
//! truncated output. click behaves similar to diff click - popup with full card"*.
//!
//! The row is the daemon's `completion_notice` (`crates/harnessd/src/harness.rs`), a `[job] …`
//! user row spoken by the harness, and **it is the model's text**: what the model reads is not
//! touched here. This is a rendering of it, the way [`folded_notice`] is — and it exists beside
//! that fold because the fold could not account for the notice the operator pasted: a command
//! written over several lines and the `memory:` line under it are both text the fold does not
//! recognise, so it handed the whole row back raw, R7's paragraph to the model included.
//!
//! # The two lines
//!
//! ```text
//! j1042 · build-and-test-null · exited 0 2m54s · bash -c 'cd /home/dead/Projects/dbtest/pg-n…
//!   ⎿ test result: ok. 12 passed; 0 failed
//! ```
//!
//! The first is the job — id, the name the agent gave it, how it ended and how long it took,
//! and the command, flattened to one line and cut to the width. The second is **the last line
//! the job wrote**: a build or a test run says how it went at its end (`test result: ok`,
//! `error[E0425]`, `=== done`), and its head is the command echoing itself or a compiler's
//! preamble. The output is not in the notice — R7 keeps the bytes one `job_output` away — so
//! the head reads the job's tail the way the jobs pane reads a window (`ReadJobOutput`), once
//! per job and only for a notice that is on the screen; until the answer lands, and for a job
//! whose output this session cannot read, the line says what the notice itself knows: what the
//! job wrote and its memory.
//!
//! # Only the shape this head KNOWS is drawn compact
//!
//! [`folded_notice`]'s rule, kept: every line of the notice is accounted for — the heading is one
//! of the four `completion_notice` writes, the bullet count matches the heading's, every bullet
//! parses, the only text between bullets is a command's continuation and its `memory:` line, and
//! the closing paragraph is one of the two the daemon writes. Anything else is `None`, and the
//! row is drawn as it was before this existed. The parser is pinned against the daemon's own
//! output (`app/tests/fixtures/job_notice_*.txt`, which `harnessd`'s tests regenerate and
//! compare byte for byte), so a change to the sentence breaks a test before it breaks a screen.

use std::collections::HashMap;

use crate::app::SubagentState;
use crate::ui::render::trim_to;
use crate::ui::*;

/// **The closing paragraphs `completion_notice` ends a `[job]` group with**, by their opening
/// words: the owner's (R7's promise not to wait) and the one a parent is told when the session
/// that started the job has stopped.
const CLOSINGS: &[&str] = &[
    // `harnessd`'s `JOB_CLOSING` (R93.C1: the old closing was a paragraph about not waiting).
    "Output: `job_output` ",
    "This session is told because the one that started it cannot be:",
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

/// **One job a notice reports ended**, as the bullet says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndedJob {
    /// The handle, `j1042` — what `ReadJobOutput` takes.
    pub(crate) id: String,
    /// The name the agent gave it, when it gave one.
    pub(crate) slug: Option<String>,
    /// The process's own word — `exited 0`, `killed by job_kill`.
    pub(crate) state: String,
    /// How long it ran, in the daemon's spelling (`2m54s`, `4.4s`).
    pub(crate) after: String,
    /// Bytes it wrote, all streams.
    pub(crate) produced: u64,
    /// The command, verbatim — several lines when it was written over several.
    pub(crate) command: String,
    /// `memory: …`'s clause, when the notice carried one.
    pub(crate) memory: Option<String>,
    /// The session that started it, when it is not this one (the handed-up notice).
    pub(crate) owner: Option<String>,
}

/// **A `[job]` notice group, parsed** — whether it is this session's own (`mine`) and the jobs
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
    // **The closing paragraph is the group's end**, found by its words rather than by a blank
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
    // **The closing is one paragraph**, followed at most by the line naming the plan rows that
    // wait on these jobs ([`waiting_line`]); any other line after it is something this head was
    // not told about.
    let closing = match closing.split_once('\n') {
        None => closing,
        Some((closing, after)) if is_waiting_line(after) => closing,
        Some(_) => return None,
    };
    // Which closing goes with which heading is the daemon's rule, and a mismatch is a shape this
    // head does not know.
    if closing.starts_with(CLOSINGS[0]) != mine {
        return None;
    }
    let mut jobs: Vec<EndedJob> = Vec::new();
    for line in body.split('\n') {
        if let Some(bullet) = line.strip_prefix("  - `") {
            jobs.push(bullet_of(bullet)?);
        } else if let Some(m) = line.strip_prefix("    memory: ") {
            let job = jobs.last_mut()?;
            if job.memory.is_some() {
                return None;
            }
            job.memory = Some(m.to_string());
        } else {
            // **A command's own next line** — only before the memory clause, which the daemon
            // writes after the bullet's last line.
            let job = jobs.last_mut()?;
            if job.memory.is_some() {
                return None;
            }
            job.command.push('\n');
            job.command.push_str(line);
        }
    }
    if jobs.len() != want {
        return None;
    }
    // **Whose it was**, on the end of the command's last line, for a handed-up notice only.
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

/// **The opening words of the daemon's line naming the plan rows that wait on a completion** —
/// `harnessd`'s `WAITING_ON_THIS` / `WAITING_ON_THESE`, which it writes as the last line of a
/// `[job]` or `[task]` group, after the closing paragraph.
pub(crate) const WAITING: &[&str] = &["Waiting on this: ", "Waiting on these: "];

/// Whether `line` is exactly one such line.
pub(crate) fn is_waiting_line(line: &str) -> bool {
    !line.contains('\n') && WAITING.iter().any(|w| line.starts_with(w))
}

/// **The waiting line of a notice group, folded for a person**: the rows it names, without the
/// instruction to the model that follows the dash — `Waiting on this: "make tier 1 green" (the
/// operator's)`. `None` when the group has no such line.
pub(crate) fn waiting_line(group: &str) -> Option<String> {
    let line = group.lines().rev().find(|l| is_waiting_line(l))?;
    Some(folded_waiting(line))
}

/// One waiting line, cut before ` — update …`, which is addressed to the model.
pub(crate) fn folded_waiting(line: &str) -> String {
    match line.rfind(" — update ") {
        Some(at) => line[..at].to_string(),
        None => line.to_string(),
    }
}

/// The first line of a bullet, after `  - `` ` ``: `LABEL` STATE after DUR, wrote N bytes: CMD.
fn bullet_of(bullet: &str) -> Option<EndedJob> {
    let (label, said) = bullet.split_once('`')?;
    let said = said.strip_prefix(' ')?;
    let (ended, wrote) = said.split_once(", wrote ")?;
    let (state, after) = ended.rsplit_once(" after ")?;
    let (bytes, command) = wrote.split_once(" bytes: ")?;
    let produced = bytes.parse::<u64>().ok()?;
    // **The label is `job_label`'s**: `slug · j65`, or the bare id when nobody named the job.
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
    })
}

fn is_job_id(s: &str) -> bool {
    s.strip_prefix('j')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// **What this head holds of a job's output, for the notice's second line and its popup.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobTail {
    /// Asked for; the answer has not landed.
    Asked,
    /// The last lines the job wrote — a window that reached the end of its output.
    Lines(Vec<String>),
    /// The daemon cannot show it (the job was reaped and only its record is left, or the read
    /// was refused); the reason, in its words.
    Gone(String),
}

/// How much of a job's end the head asks for: enough for the popup to show a screen of it.
pub(crate) const TAIL_BYTES: u64 = 4096;

/// **The groups of a notice row**, split where a new labelled notice starts. The daemon joins the
/// notices of one wake with a blank line (`completion_notices`), and a `[job]` group ends at its
/// own closing paragraph — so a blank line inside a command does not split it.
pub(crate) fn notice_groups(text: &str) -> Vec<&str> {
    // `[meanwhile] ` is the line R79's held batch opens with; the groups under it are the
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

/// **The jobs a notice row reports**, across every `[job]` group in it — `(mine, job)` — or
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

/// **The compact row**: two lines per ended job, every other group of the row folded the way
/// [`folded_notice`] folds it. `None` when any group is a shape this head does not know — the
/// row is then drawn as the fold or as it arrived, exactly as before.
///
/// Lines are cut to `cols` with the head's own `…`.
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

/// **One job's two lines**, uncut.
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
    first.push_str(" · ");
    first.push_str(&flat(&j.command));
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
            let mut facts = if j.produced == 0 {
                "wrote nothing".to_string()
            } else {
                format!("wrote {} bytes", j.produced)
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
    [first, format!("  ⎿ {second}")]
}

/// Whitespace, newlines included, collapsed to single spaces — a command written over ten lines
/// is one line of a row.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
