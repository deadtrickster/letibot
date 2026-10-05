//! The gatekeeper: the reviewer that judges a subagent's work before it can land.
//!
//! The operator's ask, in their words:
//!
//! > *"we need a gatekeeper - a subagent that does code review on merge queue -
//! > reads original brief only and then accesses - so it is kinda a permanent
//! > subagents that lazily starts as soon as task_start was finished and item put
//! > into the queue"*
//!
//! # What this module is
//!
//! The gatekeeper's own half of the merge queue. The queue does not exist yet —
//! it is being built in its own branch — and this is everything the queue needs to
//! wake a reviewer: the seat ([`crate::runtime::roles::gatekeeper`]), the brief-first
//! request ([`ReviewRequest`]), the verdict ([`Verdict`]), and the prompt
//! ([`review_prompt`]). The one function the queue calls is [`wake`], and it is a
//! seam: it refuses by name until the queue lands.
//!
//! # What this module deliberately does not do
//!
//! - **No queue.** The merge queue is being built in its own branch, and this
//!   module is the half it wakes. [`wake`] is the seam, marked, and it refuses by
//!   name until the queue lands.
//! - **No storage.** There is no table of entries here. An entry is the queue's;
//!   this module takes one, as a [`ReviewRequest`], and says what a reviewer asked
//!   with it would be told.

/// The request the gatekeeper is given: the original brief, verbatim, and the
/// placement facts.
///
/// This is the brief-first protocol made structural. The gatekeeper is given the
/// *original brief* the child was spawned with — the prompt text, verbatim — and
/// the placement facts (branch, base SHA). It is deliberately **not** given the
/// child's report: a child's report is its own framing of the ask, and a reviewer
/// that starts from that framing reviews the story rather than the artifact.
///
/// There is no field for a report, so a report cannot be smuggled in. The prompt
/// ([`review_prompt`]) is a function of exactly these three fields, and nothing
/// else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    /// The prompt the child was spawned with, verbatim. The only framing the
    /// reviewer is allowed to start from.
    pub brief: String,
    /// The branch the work landed on. Named in the verdict, because a verdict
    /// against a branch nobody can name is not evidence.
    pub branch: String,
    /// The base SHA the branch was cut from. The diff is `base...branch`, and the
    /// verdict names it for the same reason.
    pub base_sha: String,
}

/// The decision the gatekeeper reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The artifact does what the brief asked, without breaking what was there.
    Accept,
    /// It does not, and the reasons say why.
    Reject,
    /// The reviewer cannot decide from what it can see; a person must.
    NeedsHuman,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::Reject => "reject",
            Decision::NeedsHuman => "needs_human",
        }
    }
}

/// What the reviewer actually looked at: the files it read and the commands it ran.
///
/// A verdict is evidence, and evidence says what it looked at. A verdict that
/// names no files and runs no commands is an opinion, and this is the field that
/// keeps the two apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LookedAt {
    /// The files the reviewer read.
    pub files: Vec<String>,
    /// The commands the reviewer ran — `git diff`, `git log`, and the like.
    pub commands: Vec<String>,
}

/// The verdict: the entry judged, the decision, the reasons, and the evidence.
///
/// A verdict is evidence, so it must say which branch and base it judged: a
/// verdict against a SHA nobody can name is not evidence. The branch and base come
/// from the [`ReviewRequest`] the reviewer was asked about
/// ([`Verdict::for_request`]), so a verdict can never name a SHA the queue did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The branch judged.
    pub branch: String,
    /// The base SHA judged.
    pub base_sha: String,
    /// The decision.
    pub decision: Decision,
    /// The reasons, in the reviewer's words.
    pub reasons: Vec<String>,
    /// What the reviewer actually looked at.
    pub looked_at: LookedAt,
}

impl Verdict {
    /// A verdict against the request it was asked about: the branch and base come
    /// from the request, so a verdict can never name a SHA the queue did not.
    pub fn for_request(
        req: &ReviewRequest,
        decision: Decision,
        reasons: Vec<String>,
        looked_at: LookedAt,
    ) -> Self {
        Verdict {
            branch: req.branch.clone(),
            base_sha: req.base_sha.clone(),
            decision,
            reasons,
            looked_at,
        }
    }

    /// The operator-visible rendering, in the house voice.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "gatekeeper verdict\n  branch   {}\n  base     {}\n  decision {}\n",
            self.branch,
            self.base_sha,
            self.decision.as_str()
        ));
        out.push_str("  reasons\n");
        if self.reasons.is_empty() {
            out.push_str("    (none)\n");
        } else {
            for r in &self.reasons {
                out.push_str(&format!("    - {r}\n"));
            }
        }
        out.push_str("  looked at\n");
        if self.looked_at.files.is_empty() && self.looked_at.commands.is_empty() {
            out.push_str("    (nothing named)\n");
        } else {
            if !self.looked_at.files.is_empty() {
                out.push_str(&format!(
                    "    files    {}\n",
                    self.looked_at.files.join(", ")
                ));
            }
            if !self.looked_at.commands.is_empty() {
                out.push_str(&format!(
                    "    commands {}\n",
                    self.looked_at.commands.join("; ")
                ));
            }
        }
        out
    }
}

/// The exact text the gatekeeper is asked with: the brief, the placement, and the
/// rules.
///
/// The rules are the protocol in the reviewer's own prompt: read the code, do not
/// fix it, judge the artifact against the ask, and say what you looked at. The
/// brief is the only framing, and it is there verbatim; the placement names the
/// branch and base the verdict must carry back.
pub fn review_prompt(req: &ReviewRequest) -> String {
    format!(
        "You are the gatekeeper. A subagent has finished work on a branch, and you \
         judge whether it may land. You were not given the subagent's report, and \
         you will not get one: you review the artifact against the ask, not the \
         story the author tells about it.\n\n\
         The ask, verbatim — this is what the work was for, and it is the only \
         framing you start from:\n\n\
         {brief}\n\n\
         The placement:\n\
         - branch: {branch}\n\
         - base:   {base}\n\n\
         The rules:\n\
         1. Read the code. `git diff {base}...{branch}` is the change; the files it \
         touches are the artifact. Read them, and whatever they call.\n\
         2. Do not fix it. You have no write tool and you will not ask for one. A \
         reviewer that can edit becomes a second author, and then nobody reviewed.\n\
         3. Judge the artifact against the ask. The question is not \"is this good \
         code\" and not \"did the subagent do what it said it would do\"; it is \
         \"does this change do what the brief asked for, without breaking what was \
         already there\".\n\
         4. Say what you looked at. Your verdict must name the files you read and \
         the commands you ran, against the branch and base above. A verdict against \
         a SHA nobody can name is not evidence.\n\n\
         End with your verdict: accept, reject, or needs_human — the reasons, and \
         what you looked at.",
        brief = req.brief,
        branch = req.branch,
        base = req.base_sha,
    )
}

/// **The seam the merge queue lands on.**
///
/// The queue does not exist yet — it is being built in its own branch — and this
/// is the one function it will call: an entry lands, the queue hands the entry's
/// brief and placement facts here, and a gatekeeper session starts under
/// [`crate::runtime::roles::gatekeeper`], asked with [`review_prompt`].
///
/// "Permanent, lazily started" is a session the queue wakes: the seat and the
/// protocol are ready for a wake, and nothing in this module requires the queue to
/// exist. This function is the wake, and until the queue lands it refuses by name
/// rather than pretending to have started a reviewer.
// TODO(queue): the entry that wakes this
pub fn wake(req: ReviewRequest) -> Result<String, String> {
    Err(format!(
        "the merge queue is not built yet, so nothing can wake the gatekeeper for \
         branch `{}` at base `{}`. The seat (roles::gatekeeper) and the protocol \
         (review_prompt) are ready; the queue is the missing half.",
        req.branch, req.base_sha
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ReviewRequest {
        ReviewRequest {
            brief: "add a gatekeeper seat and a brief-first protocol".into(),
            branch: "agent/gatekeeper".into(),
            base_sha: "a4fbe70".into(),
        }
    }

    /// The brief is the only framing, and it is there verbatim; the placement is
    /// named; and the prompt says the report is not given. The report's absence as
    /// data is structural — the request has no field for one — so the test asserts
    /// the three fields' presence and the stated negative rather than searching for
    /// a report that the type cannot carry.
    #[test]
    fn the_prompt_carries_the_brief_verbatim_and_no_report() {
        let req = request();
        let p = review_prompt(&req);
        assert!(
            p.contains(&req.brief),
            "the brief must appear verbatim:\n{p}"
        );
        assert!(p.contains(&req.branch), "the branch must be named:\n{p}");
        assert!(p.contains(&req.base_sha), "the base must be named:\n{p}");
        assert!(
            p.contains("You were not given the subagent's report"),
            "the prompt must say the report is not given:\n{p}"
        );
    }

    /// The verdict names the branch and base it judged, and what it looked at.
    #[test]
    fn the_verdict_names_the_branch_the_base_and_what_it_saw() {
        let req = request();
        let v = Verdict::for_request(
            &req,
            Decision::Reject,
            vec!["the brief asked for X; the change does Y".into()],
            LookedAt {
                files: vec!["crates/tools/src/gatekeeper.rs".into()],
                commands: vec!["git diff a4fbe70...agent/gatekeeper".into()],
            },
        );
        let r = v.render();
        assert!(r.contains("agent/gatekeeper"), "{r}");
        assert!(r.contains("a4fbe70"), "{r}");
        assert!(r.contains("reject"), "{r}");
        assert!(r.contains("crates/tools/src/gatekeeper.rs"), "{r}");
        assert!(r.contains("git diff a4fbe70...agent/gatekeeper"), "{r}");
    }

    /// The seam refuses by name until the queue lands.
    #[test]
    fn the_wake_refuses_by_name_until_the_queue_lands() {
        let e = wake(request()).unwrap_err();
        assert!(e.contains("merge queue"), "{e}");
        assert!(e.contains("agent/gatekeeper"), "{e}");
    }
}
