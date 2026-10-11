//! **A red merge target, drawn as ONE line** — and the whole of what the daemon said one click
//! away, in the popup the job card opens.
//!
//! The operator, pasting what one red `main` drew — a `target_red` warning that wrapped into a
//! paragraph with the gate's output under it, and then the session's own `EMERGENCY —` notice to
//! the model with the same output again in a fenced block: *"please make all this: … a one-line
//! similar to job - with a popup"*. Two big blocks for one event.
//!
//! # The lines
//!
//! ```text
//! ‼ main RED at 60bf819c · firecode · bin/firecode test failed · click for details
//! ‼ main RED at 60bf819c · firecode · bin/firecode test failed (step 1/1) · review red-main-60bf819c3342 · click for details
//! ```
//!
//! The first is the hub warning (`target_red`, raised by the queue's `target_went_red`); the
//! second is the notice the project's root session reads (`mergequeue::notices::health`), which
//! also numbers the failing step and names the red tip's review. Both are cut to the width with
//! the head's `…`, and both are drawn in the failure register, the queue pane's `‼ EMERGENCY`.
//! The two siblings of the session notice — the gate TIMED OUT, and the gate CANNOT JUDGE the tip
//! because its own setup failed — get the same line with their own word, and so do the
//! `target_red` warnings the red tip's reviewer raises (`finds `main` broken at …`, with or
//! without a fix branch). The daemon raises nothing when the target turns green again (it only
//! withdraws the stale reviews), so there is no recovery line to pair with these.
//!
//! # Only what this head can read is folded
//!
//! [`folded_notice`]'s rule and `compact_notice`'s: the head of the text must be exactly one of
//! the daemon's openings, with a target, a sha and a repository in it. Anything else is `None`
//! and the row is drawn as it was before this existed. **But the parser keys on the stable
//! parts only** — the opening, the target, the sha, the repository, the failing step — and
//! tolerates sentences it does not know after them: the daemon is expected to grow a sentence
//! about what the landing tree lacks, and a new sentence must not cost the operator the one
//! line. Nothing is lost by tolerating it: the line is a summary, and the popup is the text
//! verbatim.
//!
//! The record is untouched: the model reads the daemon's words, every one of them.

use std::collections::HashMap;

use crate::app::SubagentState;
use crate::ui::render::trim_to;
use crate::ui::transcript::job_card::{JobTail, compact_notice};
use crate::ui::*;

/// The mark the line opens with — the queue pane's own for an emergency.
pub(crate) const ALARM: &str = "‼";

/// What the line ends with: the row is a summary, and the popup is the text.
const CLICK: &str = "click for details";

/// **The last words of a session notice** — `TOOL_WAYS`' closing clause in
/// `mergequeue::notices`, which every `health` notice ends with. A row of several notices is
/// split after it.
const NOTICE_END: &str = "the operator, whose decision it is.";

/// What is wrong with the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trouble {
    /// Its merge gate fails at its tip.
    Red,
    /// Its gate ran out of time, so the tip is unjudged.
    TimedOut,
    /// The gate's own setup failed, so it never judged the tip.
    Unjudged,
}

/// **One red (or unjudged) target, as the daemon's text says it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedTip {
    pub(crate) trouble: Trouble,
    /// The branch, `main`.
    pub(crate) target: String,
    /// The tip, as the text spells it (twelve characters in a notice, forty in a warning).
    pub(crate) sha: String,
    /// The repository's path, when the text names it.
    pub(crate) repo: Option<String>,
    /// The failing step's command, when the text names it.
    pub(crate) step: Option<String>,
    /// Which step of how many, when the text numbers it.
    pub(crate) of: Option<(usize, usize)>,
    /// The red tip's review row, `red-main-60bf819c3342`.
    pub(crate) review: Option<String>,
    /// Who judged the tip broken, for a reviewer's warning.
    pub(crate) by: Option<String>,
    /// What came of that verdict — a fix branch opened, or not.
    pub(crate) outcome: Option<String>,
}

impl RedTip {
    fn new(trouble: Trouble, target: &str, sha: &str) -> Option<RedTip> {
        let sha = sha.trim();
        if target.is_empty()
            || target.contains('\n')
            || sha.len() < 7
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        Some(RedTip {
            trouble,
            target: target.to_string(),
            sha: sha.to_string(),
            repo: None,
            step: None,
            of: None,
            review: None,
            by: None,
            outcome: None,
        })
    }

    /// `‼ main RED at 60bf819c` — the part drawn in the failure register.
    pub(crate) fn head(&self) -> String {
        let short: String = self.sha.chars().take(8).collect();
        let word = match self.trouble {
            Trouble::Red => "RED",
            Trouble::TimedOut => "gate TIMED OUT",
            Trouble::Unjudged => "CANNOT BE JUDGED",
        };
        format!("{ALARM} {} {word} at {short}", self.target)
    }

    /// **The whole line, uncut**: the head, the repository's name, the failing step, the review
    /// or the verdict, and where the rest is.
    pub(crate) fn line(&self) -> String {
        let mut parts = vec![self.head()];
        if let Some(r) = &self.repo {
            parts.push(repo_name(r));
        }
        if let Some(by) = &self.by {
            parts.push(format!("{by} finds it broken"));
        }
        if let Some(s) = &self.step {
            let word = match self.trouble {
                Trouble::TimedOut => "timed out",
                Trouble::Red | Trouble::Unjudged => "failed",
            };
            let mut p = format!("{} {word}", flat(s));
            if let Some((n, of)) = self.of {
                p.push_str(&format!(" (step {n}/{of})"));
            }
            parts.push(p);
        } else if self.trouble == Trouble::Unjudged {
            parts.push("the gate's setup failed".into());
        }
        if let Some(r) = &self.review {
            parts.push(format!("review {r}"));
        }
        if let Some(o) = &self.outcome {
            parts.push(o.clone());
        }
        parts.push(CLICK.into());
        parts.join(" · ")
    }

    /// The popup's top edge: the head and the repository.
    pub(crate) fn title(&self) -> String {
        match &self.repo {
            Some(r) => format!("{} · {}", self.head(), repo_name(r)),
            None => self.head(),
        }
    }
}

/// The last component of a repository's path — the queue pane's `repo_name`.
fn repo_name(repo: &str) -> String {
    std::path::Path::new(repo.trim_end_matches('/'))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.to_string())
}

fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **A `target_red` warning's detail, read** — `None` for a shape this head does not know.
///
/// Two families, both `EMERGENCY: ` and then:
///
/// - the queue's own (`RepoQueue::target_went_red`): `` `main` is RED at SHA in REPO. The merge
///   queue lands nothing … `bin/firecode test` failed:\n<output> ``;
/// - the red tip's reviewer's (`review::open_fix_branch`): `` BY finds `main` broken at SHA:
///   REASONS. `` and then one of three endings — a fix branch opened, a fix branch that could
///   not be, or the project's `red_target = "raise"`.
pub(crate) fn warning_tip(detail: &str) -> Option<RedTip> {
    let rest = detail.strip_prefix("EMERGENCY: ")?;
    if let Some(r) = rest.strip_prefix('`') {
        let (target, r) = r.split_once("` is RED at ")?;
        let (sha, r) = r.split_once(" in ")?;
        // The repository runs to the end of its sentence; the line is one line.
        let (repo, _) = r.split_once(". ")?;
        if repo.contains('\n') || repo.is_empty() {
            return None;
        }
        let mut tip = RedTip::new(Trouble::Red, target, sha)?;
        tip.repo = Some(repo.to_string());
        // The evidence is the gate's verdict: `` `STEP` failed:\n<output> ``.
        if let Some(at) = detail.find("` failed:\n") {
            let before = &detail[..at];
            if let Some(open) = before.rfind('`') {
                let step = &before[open + 1..];
                if !step.is_empty() && !step.contains('\n') {
                    tip.step = Some(step.to_string());
                }
            }
        }
        return Some(tip);
    }
    let (by, r) = rest.split_once(" finds `")?;
    let (target, r) = r.split_once("` broken at ")?;
    let (sha, r) = r.split_once(": ")?;
    let mut tip = RedTip::new(Trouble::Red, target, sha)?;
    if by.is_empty() || by.contains('\n') {
        return None;
    }
    tip.by = Some(by.to_string());
    tip.outcome = Some(if let Some(at) = r.rfind(". A fix branch `") {
        let t = &r[at + ". A fix branch `".len()..];
        let (branch, t) = t.split_once("` was opened by ")?;
        let handle = t.split_whitespace().next()?;
        format!("fix branch {branch} opened by {handle}")
    } else if r.contains(". A fix branch could NOT be opened: ") {
        "NO fix branch could be opened".to_string()
    } else if r.contains("`red_target = \"raise\"`") {
        "no fix branch: a person fixes it".to_string()
    } else {
        return None;
    });
    Some(tip)
}

/// **The opening paragraph of a session notice**, read: `mergequeue::notices::health`'s three
/// openings. Only the first paragraph is read for the target, the sha and the repository, so a
/// sentence added after it changes nothing.
fn session_head(text: &str) -> Option<RedTip> {
    let first = text.split("\n\n").next()?;
    let (trouble, rest) = if let Some(r) = first.strip_prefix("EMERGENCY — `") {
        (Trouble::Red, r)
    } else if let Some(r) = first.strip_prefix("The merge queue's gate of `") {
        (Trouble::TimedOut, r)
    } else if let Some(r) = first.strip_prefix("The merge queue CANNOT JUDGE `") {
        (Trouble::Unjudged, r)
    } else {
        return None;
    };
    let (target, r) = rest.split_once("` at ")?;
    let (sha, r) = r.split_once(" in ")?;
    let end = match trouble {
        Trouble::Red => " is RED: ",
        Trouble::TimedOut => " TIMED OUT, ",
        Trouble::Unjudged => ": ",
    };
    let (repo, _) = r.split_once(end)?;
    if repo.is_empty() || repo.contains('\n') {
        return None;
    }
    let mut tip = RedTip::new(trouble, target, sha)?;
    tip.repo = Some(repo.to_string());
    Some(tip)
}

/// **A session notice, read** — the opening, and from anywhere in it the failing step
/// (`The failing step is 1 of 1, `bin/firecode test`.`) and the red tip's review
/// (`` first in the review queue (`red-main-…`) ``).
pub(crate) fn notice_tip(text: &str) -> Option<RedTip> {
    let mut tip = session_head(text)?;
    if let Some(at) = text.find("The failing step is ") {
        let r = &text[at + "The failing step is ".len()..];
        let parsed = (|| {
            let (n, r) = r.split_once(" of ")?;
            let (of, r) = r.split_once(", `")?;
            let (cmd, _) = r.split_once("`. The end of its output:")?;
            Some((n.parse().ok()?, of.parse().ok()?, cmd))
        })();
        if let Some((n, of, cmd)) = parsed {
            tip.step = Some(cmd.to_string());
            tip.of = Some((n, of));
        }
    }
    if let Some(at) = text.find("first in the review queue (`") {
        let r = &text[at + "first in the review queue (`".len()..];
        if let Some((id, _)) = r.split_once("`)")
            && !id.is_empty()
            && !id.contains(char::is_whitespace)
        {
            tip.review = Some(id.to_string());
        }
    }
    Some(tip)
}

/// **Where a row's first notice ends**, when the row opens with one: `(the notice, the rest)`.
///
/// The daemon joins the notices of one wake with a blank line, and a gate's output — inside the
/// notice — can hold blank lines of its own, so the split is at the notice's last words and
/// only when what follows is another notice this head knows how to open (a labelled one, or a
/// second emergency). Anything else after it is kept INSIDE the card: an extra sentence the
/// daemon learned to say is the notice's, and the popup shows it.
pub(crate) fn split_notice(text: &str) -> Option<(&str, &str)> {
    session_head(text)?;
    if let Some(at) = text.find(NOTICE_END) {
        let end = at + NOTICE_END.len();
        if let Some(next) = text[end..].strip_prefix("\n\n")
            && (next.starts_with('[') || session_head(next).is_some())
        {
            return Some((&text[..end], next));
        }
    }
    Some((text, ""))
}

/// **Whether a row is one this module draws as a line** — the click map's question, which
/// needs no width.
pub(crate) fn is_red_row(text: &str) -> bool {
    split_notice(text).is_some()
}

/// The popup's top edge for a red row or warning, if `text` is one.
pub(crate) fn popup_title(text: &str) -> Option<String> {
    split_notice(text)
        .and_then(|(n, _)| notice_tip(n))
        .or_else(|| warning_tip(text))
        .map(|t| t.title())
}

/// **The compact row**: one line per emergency notice, and every other notice of the row drawn
/// the way it is drawn without one (the job card, or the fold). `None` when the row does not
/// open with an emergency, or when any part of it is a shape this head does not know — the row
/// is then drawn as it was.
pub(crate) fn compact_emergency(
    text: &str,
    subagents: &[SubagentState],
    tails: &HashMap<String, JobTail>,
    cols: usize,
) -> Option<Vec<String>> {
    let (notice, rest) = split_notice(text)?;
    let tip = notice_tip(notice)?;
    let mut out = vec![trim_to(&tip.line(), cols)];
    if !rest.is_empty() {
        if let Some(more) = compact_emergency(rest, subagents, tails, cols) {
            out.extend(more);
        } else if let Some(more) = compact_notice(rest, subagents, tails, cols) {
            out.extend(more);
        } else {
            for l in folded_notice(rest, subagents)?.lines() {
                out.push(trim_to(l, cols));
            }
        }
    }
    Some(out)
}

/// **A `target_red` warning's one line**, cut to `width`, or `None` for a detail this head does
/// not read (the note is then drawn as it always was).
pub(crate) fn warning_line(code: &str, detail: &str, width: usize) -> Option<String> {
    if code != "target_red" {
        return None;
    }
    Some(trim_to(&warning_tip(detail)?.line(), width))
}
