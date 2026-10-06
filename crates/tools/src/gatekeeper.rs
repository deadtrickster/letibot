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

    /// **Every decision, in the order the queue reads them** — the one list, for the reason
    /// [`letibot_tokencore::store::MergePriority::ALL`] is one: the parse and the rendering
    /// both ask *which of the values is this*, and a second list is a second answer.
    ///
    /// The order is also the queue's: an `Accept` is the only one that lands, and the other
    /// two are parked for a person, which is why `Accept` is first and the two refusals are
    /// in the order a reviewer is likelier to mean them.
    pub const ALL: [Decision; 3] = [Decision::Accept, Decision::Reject, Decision::NeedsHuman];

    /// The decision a stored word names, if it names one.
    ///
    /// `None` for a word outside the set, and that is a refusal rather than a default: a
    /// verdict whose word nobody knows is a verdict the queue cannot act on, and reading it
    /// as `Accept` would land a branch on the strength of a typo.
    pub fn parse(stored: &str) -> Option<Decision> {
        Self::ALL.into_iter().find(|d| d.as_str() == stored)
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
///
/// # The closing block is a SHAPE, and that is a change
///
/// This prompt used to end *"End with your verdict: accept, reject, or needs_human —
/// the reasons, and what you looked at"*, which asks a model for prose and then leaves
/// whoever reads it to decide what the prose meant. That is exactly the defect this
/// tree's own store warns about — *"re-parsing prose to recover a label is how a corpus
/// rots"* — and it would be worse here than in a corpus: the label the queue reads is
/// the one that decides whether a branch lands.
///
/// So the prompt states the shape the answer must close with, field by field, and
/// [`parse_verdict`] reads THAT. A reply that does not close with it is refused by name
/// rather than guessed at, and the refusal is the safe direction: a verdict nobody can
/// read is a verdict the queue does not have, and the entry waits.
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
         You may reason in prose for as long as you like. **Then end your reply with \
         exactly this block and nothing after it**, one field per line, the labels \
         spelled exactly as they are here:\n\n\
         verdict: accept | reject | needs_human\n\
         reasons: one reason per line, each beginning with `- `\n\
         files: comma-separated paths, or `none`\n\
         commands: semicolon-separated commands, or `none`\n\n\
         `verdict:` is the only line that is required; `reasons:`, `files:` and \
         `commands:` are read as evidence and an empty one is recorded as nothing \
         looked at, which is a worse verdict rather than a refused one. A reply \
         whose closing block is missing or whose verdict is not one of the three \
         words is refused, and the branch does not land.",
        brief = req.brief,
        branch = req.branch,
        base = req.base_sha,
    )
}

/// **The seam the merge queue lands on.**
///
/// The queue hands an entry's brief and placement facts here, and a gatekeeper session is
/// asked with [`review_prompt`]. The two halves are two different layers, and that is what
/// the door below is for: **this crate has no daemon.** A wake is two acts on the daemon's
/// side — write the request down where the reviewer's session will find it, and ring the bell
/// that starts that session's turn — and neither is expressible here, so [`Reviewer`] is the
/// caller's own door and this function is the protocol's precondition.
///
/// # What this function refuses, and why it is not a formality
///
/// A request with an empty brief, branch or base is a request nobody can act on, and it is
/// refused BY NAME rather than handed on: a reviewer given no ask would review the artifact
/// against nothing and its verdict would be an opinion about the code, which is exactly what
/// the brief-first protocol exists to prevent. A base SHA nobody can name is the same failure
/// one field over — a verdict against a SHA that cannot be resolved is not evidence.
pub fn wake(entry_id: &str, req: ReviewRequest, reviewer: &dyn Reviewer) -> Result<String, String> {
    if entry_id.trim().is_empty() {
        return Err(
            "a review was asked for without an entry id, so the verdict could not be attached \
             to the entry it is about. Nothing was asked."
                .into(),
        );
    }
    if req.brief.trim().is_empty() {
        return Err(format!(
            "the entry `{entry_id}` carries no brief, so there is nothing for the gatekeeper to \
             review `{}` AGAINST — and a reviewer given no ask reviews the code against nothing. \
             Nothing was asked.",
            req.branch
        ));
    }
    if req.branch.trim().is_empty() || req.base_sha.trim().is_empty() {
        return Err(format!(
            "the entry `{entry_id}` names no branch or no base SHA, so a verdict about it could \
             not name what it judged — and a verdict against a SHA nobody can name is not \
             evidence. Nothing was asked."
        ));
    }
    reviewer.wake(entry_id, &req)
}

/// **The door a wake goes through** — the daemon's own two acts, and nothing else.
///
/// `wake` is called on the queue's thread, which owns no session and no hub; the reviewer is a
/// session the daemon serves, so the ask is a fact written down plus a bell rung, and the
/// answer comes back through the store on a later pass. That asynchrony is the point rather
/// than a limitation: the queue is serial and the review is minutes long, so blocking the
/// queue's thread on a turn would stop it serving anything else — and a daemon that restarted
/// mid-review would have nothing to come back to.
///
/// A door that cannot write the request or ring the bell returns `Err`, and the queue reports
/// it rather than pretending a reviewer was asked: an entry waiting for a verdict nobody was
/// asked for waits for ever, and *that* is the failure this refusal exists to make impossible.
pub trait Reviewer {
    /// **Write the request down and ring the reviewer's bell.** `Ok` is what was done, in the
    /// words the operator gets; `Err` is why it could not be, said rather than swallowed.
    fn wake(&self, entry_id: &str, req: &ReviewRequest) -> Result<String, String>;
}

/// **A door that refuses by name** — what a build with no reviewer session attached answers.
///
/// The default a daemon wires in when it has no store to write a request to, and the honest
/// answer for a caller that asked anyway: an entry that waits for a verdict nobody was asked
/// for waits for ever, so *nothing was asked* has to be said out loud rather than reported as
/// a wake.
pub struct NoReviewer;

impl Reviewer for NoReviewer {
    fn wake(&self, entry_id: &str, req: &ReviewRequest) -> Result<String, String> {
        Err(format!(
            "this daemon has no gatekeeper session attached, so nothing was asked about \
             `{entry_id}` (branch `{}`, base `{}`). The branch does not land without a verdict.",
            req.branch, req.base_sha
        ))
    }
}

/// **What the reviewer said, read out of the shape the prompt asks for** — the one place a
/// reply becomes a verdict.
///
/// The prompt states a closing block (`verdict:`, `reasons:`, `files:`, `commands:`) and this
/// reads exactly that, from the LAST `verdict:` line onwards: prose above it is the reviewer
/// thinking, which is worth reading in the session's transcript and is not evidence.
///
/// **A reply without a readable verdict is an `Err`, never a default.** Guessing `accept`
/// would land a branch nobody accepted and guessing `reject` would park work nobody rejected;
/// both are worse than saying the verdict could not be read, which leaves the entry waiting
/// with the reason on its row — the direction the whole queue is built to fail in.
///
/// The branch and base come from the request ([`Verdict::for_request`]), so a verdict can
/// never name a SHA the queue did not hand over.
pub fn parse_verdict(req: &ReviewRequest, said: &str) -> Result<Verdict, String> {
    let lines: Vec<&str> = said.lines().collect();
    let Some(start) = lines.iter().rposition(|l| label(l, "verdict").is_some()) else {
        return Err(format!(
            "the reviewer's reply for `{}` has no `verdict:` line, so there is no verdict to \
             read. The reply is in the reviewer's own session, and the entry waits.",
            req.branch
        ));
    };
    let word = label(lines[start], "verdict").unwrap_or("").trim();
    let Some(decision) = Decision::parse(word) else {
        return Err(format!(
            "the reviewer's reply for `{}` ends with `verdict: {word}`, which is not one of \
             {}. A verdict outside the closed set is not a verdict, and the entry waits.",
            req.branch,
            Decision::ALL
                .iter()
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    // **Everything after the `verdict:` line is the block, and nothing before it is.** A reply
    // that kept talking after the block is read as if it had not: the block is the answer.
    let rest = &lines[start + 1..];
    let mut reasons: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    for line in rest {
        if let Some(v) = label(line, "reasons") {
            reasons.extend(bullets(v));
        } else if let Some(v) = label(line, "files") {
            files.extend(commas(v));
        } else if let Some(v) = label(line, "commands") {
            commands.extend(semicolons(v));
        } else if let Some(bullet) = line.trim().strip_prefix("- ") {
            // **A continuation line under `reasons:`.** The prompt asks for one reason per line,
            // and a model that writes the label once and then a list has answered the question —
            // refusing that would be refusing a reply that says exactly what was asked, one
            // layout over.
            let b = bullet.trim();
            if !b.is_empty() {
                reasons.push(b.to_string());
            }
        }
    }
    Ok(Verdict::for_request(
        req,
        decision,
        reasons,
        LookedAt { files, commands },
    ))
}

/// The value on a line that begins with `label:`, or `None`. Case-insensitive on the label and
/// anchored at the start of the line, so a mention of the word `verdict` in the reviewer's own
/// prose is not mistaken for the field.
fn label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let t = line.trim_start();
    let (head, tail) = t.split_once(':')?;
    head.trim().eq_ignore_ascii_case(label).then(|| tail.trim())
}

/// One field's worth of bullets: a single line that carries several `- ` items, or one — and
/// `none` means nothing, on [`commas`]' rule.
fn bullets(v: &str) -> Vec<String> {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for piece in t.split("- ") {
        let p = piece.trim();
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
    out
}

/// A comma-separated list, with `none` meaning nothing — which the prompt names as the way to
/// say *I looked at nothing*, and which must not become a file called `none`.
fn commas(v: &str) -> Vec<String> {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    t.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// A semicolon-separated list, on `commas`' rule including `none`.
fn semicolons(v: &str) -> Vec<String> {
    let t = v.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    t.split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
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

    /// **The prompt states the closing block it will be read by.** The reply's shape is the
    /// protocol now — see `review_prompt`'s own doc for why prose was not enough — so the four
    /// labels are asserted to be there, spelled the way `parse_verdict` looks for them.
    #[test]
    fn the_prompt_states_the_closing_block_the_parser_reads() {
        let p = review_prompt(&request());
        for l in ["verdict:", "reasons:", "files:", "commands:"] {
            assert!(p.contains(l), "the prompt must state `{l}`:\n{p}");
        }
        assert!(p.contains("accept | reject | needs_human"), "{p}");
    }

    /// **A reply in the shape the prompt asks for becomes the verdict it says** — the decision,
    /// the reasons and the evidence, against the branch and base the queue handed over.
    #[test]
    fn a_reply_in_the_stated_shape_becomes_its_verdict() {
        let req = request();
        let said = "I read the diff and the files it touches.\n\n\
                    verdict: reject\n\
                    reasons:\n\
                    - the brief asked for X and the change does Y\n\
                    - `wake` is not wired to anything\n\
                    files: crates/tools/src/gatekeeper.rs, crates/harnessd/src/mergequeue.rs\n\
                    commands: git diff a4fbe70...agent/gatekeeper; cargo test -p letibot-tools\n";
        let v = parse_verdict(&req, said).expect("the block parses");
        assert_eq!(v.decision, Decision::Reject);
        assert_eq!(v.branch, req.branch, "the branch comes from the request");
        assert_eq!(v.base_sha, req.base_sha, "so does the base");
        assert_eq!(
            v.reasons,
            vec![
                "the brief asked for X and the change does Y",
                "`wake` is not wired to anything"
            ]
        );
        assert_eq!(
            v.looked_at.files,
            vec![
                "crates/tools/src/gatekeeper.rs",
                "crates/harnessd/src/mergequeue.rs"
            ]
        );
        assert_eq!(v.looked_at.commands.len(), 2);
    }

    /// **The LAST `verdict:` line is the verdict**, and the prose above it is not read: a
    /// reviewer that thinks out loud about what it might say and then says it has answered, and
    /// a parser that took the first mention would land on a sentence about a decision the
    /// reviewer went on to change.
    #[test]
    fn the_last_verdict_line_is_the_one_that_counts() {
        let said = "I considered `verdict: accept` and then read the test file.\n\n\
                    verdict: needs_human\n\
                    reasons: - the spec is ambiguous about the retry\n";
        let v = parse_verdict(&request(), said).expect("the last line is a verdict");
        assert_eq!(v.decision, Decision::NeedsHuman);
        assert_eq!(v.reasons, vec!["the spec is ambiguous about the retry"]);
    }

    /// **A reply with no readable verdict is refused, and never guessed at.** Each of the three
    /// ways it can be unreadable is its own assertion: no block at all, a word outside the
    /// closed set, and the empty string. The safe direction is the point — an entry with no
    /// verdict waits, and it must never land on the strength of a default.
    #[test]
    fn a_reply_without_a_readable_verdict_is_refused() {
        let req = request();
        for (said, want) in [
            ("I read it and I think it is fine.", "no `verdict:` line"),
            ("verdict: probably\n", "not one of"),
            ("", "no `verdict:` line"),
        ] {
            let e = parse_verdict(&req, said).unwrap_err();
            assert!(e.contains(want), "want {want:?}, got {e:?}");
            assert!(e.contains("agent/gatekeeper"), "{e}");
        }
        // And the closed set itself has one answer per word.
        assert_eq!(Decision::parse("accept"), Some(Decision::Accept));
        assert_eq!(Decision::parse("reject"), Some(Decision::Reject));
        assert_eq!(Decision::parse("needs_human"), Some(Decision::NeedsHuman));
        assert_eq!(Decision::parse("Accept"), None, "the words are exact");
        assert_eq!(Decision::parse("maybe"), None);
    }

    /// **`none` is not a file called `none`.** The prompt names it as the way to say *I looked
    /// at nothing*, and a verdict that recorded it as evidence would be a verdict claiming to
    /// have read a file that does not exist.
    #[test]
    fn an_empty_evidence_field_is_no_evidence() {
        let v = parse_verdict(
            &request(),
            "verdict: accept\nreasons: none\nfiles: none\ncommands: none\n",
        )
        .expect("a verdict with nothing named is still a verdict");
        assert!(v.reasons.is_empty(), "{:?}", v.reasons);
        assert!(v.looked_at.files.is_empty(), "{:?}", v.looked_at.files);
        assert!(
            v.looked_at.commands.is_empty(),
            "{:?}",
            v.looked_at.commands
        );
    }

    /// **The wake hands the request to the door and reports what the door did** — and it refuses
    /// a request nobody could act on BEFORE the door is reached, which is the half that belongs
    /// to this layer.
    #[test]
    fn the_wake_goes_through_the_door_and_refuses_a_blank_ask() {
        use std::sync::Mutex;

        /// A door that records what it was asked.
        struct Recording(Mutex<Vec<(String, String)>>);
        impl Reviewer for Recording {
            fn wake(&self, entry_id: &str, req: &ReviewRequest) -> Result<String, String> {
                self.0
                    .lock()
                    .unwrap()
                    .push((entry_id.to_string(), req.branch.clone()));
                Ok(format!("asked the gatekeeper about `{}`", req.branch))
            }
        }

        let door = Recording(Mutex::new(Vec::new()));
        let said = wake("m-1", request(), &door).expect("a real request goes through");
        assert!(said.contains("agent/gatekeeper"), "{said}");
        assert_eq!(
            *door.0.lock().unwrap(),
            vec![("m-1".to_string(), "agent/gatekeeper".to_string())],
            "the door was handed the entry id and the request"
        );

        // **A blank ask never reaches the door.** Three ways to be unactionable, and each one
        // is refused with the field it is about rather than handed on to a reviewer who would
        // review the artifact against nothing.
        let blank = ReviewRequest {
            brief: "  ".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
        };
        let e = wake("m-2", blank, &door).unwrap_err();
        assert!(e.contains("no brief"), "{e}");
        let nameless = ReviewRequest {
            brief: "do the thing".into(),
            branch: "".into(),
            base_sha: "abc".into(),
        };
        let e = wake("m-3", nameless, &door).unwrap_err();
        assert!(e.contains("no branch"), "{e}");
        let e = wake("  ", request(), &door).unwrap_err();
        assert!(e.contains("without an entry id"), "{e}");
        assert_eq!(
            door.0.lock().unwrap().len(),
            1,
            "a refused request must not reach the door"
        );

        // **And a build with no reviewer says so rather than reporting a wake.**
        let e = wake("m-4", request(), &NoReviewer).unwrap_err();
        assert!(e.contains("no gatekeeper session"), "{e}");
        assert!(e.contains("does not land without a verdict"), "{e}");
    }
}
