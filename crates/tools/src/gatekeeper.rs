//! The gatekeeper's half of the merge queue: the seat
//! ([`crate::runtime::Role::gatekeeper`]), the brief-first request ([`ReviewRequest`]),
//! the verdict ([`Verdict`]) and the prompt ([`review_prompt`]).
//! Two doors face opposite ways: [`wake`] is the queue's door to a reviewer, and the
//! reviewer's door is `review_verdict`, whose [`Submission`] the daemon writes onto the
//! queue's row. The answer is the child's own act, never a label read out of its prose.

/// The request the gatekeeper is given: the original brief, verbatim, and the
/// placement facts.
///
/// The brief is the only framing because a child's report is its own account of the ask,
/// and a reviewer that starts from that account reviews the story rather than the
/// artifact. There is no field for a report, so none can be smuggled in, and
/// [`review_prompt`] is a function of these fields alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    /// The prompt the child was spawned with, verbatim: the only framing a reviewer may
    /// start from.
    pub brief: String,
    /// The branch the work landed on, named in the verdict: a verdict against a branch
    /// nobody can name is not evidence.
    pub branch: String,
    /// The base SHA the branch was cut from. The diff is `base...head`, and the verdict
    /// names it for the same reason.
    pub base_sha: String,
    /// The head under review, which the verdict is bound to. The diff is `base...head_sha`
    /// rather than `base...branch`, so a commit made while the review runs is not part of
    /// what was judged, and the queue lands only this sha. Empty is the branch as it is.
    pub head_sha: String,
    /// Where to run the review's commands: the repository on the host, or empty for the
    /// reviewer's own workspace (a VM booted on a clone of the branch).
    pub cwd: String,
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

    /// Every decision, in the order the queue reads them. One list, because the parse and
    /// the rendering both ask *which of the values is this*. `Accept` leads because it is
    /// the only one that lands; the two refusals follow in the order a reviewer means them.
    pub const ALL: [Decision; 3] = [Decision::Accept, Decision::Reject, Decision::NeedsHuman];

    /// The decision a stored word names, if it names one.
    ///
    /// `None` for a word outside the set, which is a refusal rather than a default: reading
    /// an unknown word as `Accept` would land a branch on the strength of a typo.
    pub fn parse(stored: &str) -> Option<Decision> {
        Self::ALL.into_iter().find(|d| d.as_str() == stored)
    }
}

/// What the reviewer actually looked at: the files it read and the commands it ran.
///
/// Evidence says what it looked at. A verdict that names no files and runs no commands is
/// an opinion, and this is the field that keeps the two apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LookedAt {
    /// The files the reviewer read.
    pub files: Vec<String>,
    /// The commands the reviewer ran: `git diff`, `git log`, and the like.
    pub commands: Vec<String>,
}

/// The verdict: the entry judged, the decision, the reasons, and the evidence.
///
/// A verdict is evidence, so it must say which branch and base it judged. Both come from
/// the [`ReviewRequest`] the reviewer was asked about ([`Verdict::for_request`]), so a
/// verdict can never name a SHA the queue did not hand over.
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
    /// A verdict against the request it was asked about, so it can name no SHA the queue
    /// did not.
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

/// What a reviewer hands in: the verdict as the child's own act, and not as prose somebody
/// else read.
///
/// There is no field for the branch or the base: those are the [`ReviewRequest`]'s, joined
/// at the daemon's write ([`Verdict::for_request`]). This is what `review_verdict` carries
/// and the only thing that becomes a row's decision; a reply in prose is never read as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// One word out of [`Decision::ALL`]. A word outside the set is refused by the door
    /// rather than defaulted: guessing `Accept` lands a branch nobody accepted, and guessing
    /// `Reject` parks work nobody rejected.
    pub decision: Decision,
    /// Why, in the reviewer's own words.
    pub reasons: Vec<String>,
    /// What it actually looked at.
    pub looked_at: LookedAt,
}

/// The exact text the gatekeeper is asked with: the brief, the placement, and the rules.
///
/// The verdict is a `review_verdict` CALL, not prose: re-parsing a reply to recover a label
/// is a reader guessing at the decision that lands a branch. One simple command per call,
/// because the matcher never splits a compound one: a card, or `not run` with nobody there.
///
/// The prompt is the only half that can carry that rule: no seat reaches the exec path, so
/// a refusal at the seat would mean plumbing the seat into the gate first.
pub fn review_prompt(req: &ReviewRequest) -> String {
    // The revision the diffs name: the bound head when the queue named one, else the
    // branch.
    let head = if req.head_sha.trim().is_empty() {
        req.branch.clone()
    } else {
        req.head_sha.clone()
    };
    let cwd = if req.cwd.trim().is_empty() {
        String::new()
    } else {
        format!(
            "- where:  {} — the repository; give it to `bash` as `cwd` (and to `read`/`grep` as \
             the path)\n",
            req.cwd
        )
    };
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
         - head:   {head} — the commit you judge; the queue lands only this one\n\
         - base:   {base}\n\
         {cwd}\n\
         The rules:\n\
         1. Read the code. `git diff {base}...{head}` is the change; the files it \
         touches are the artifact. Read them, and whatever they call.\n\
         2. Do not fix it. You have no write tool and you will not ask for one. A \
         reviewer that can edit becomes a second author, and then nobody reviewed.\n\
         3. Judge the artifact against the ask. The question is not \"is this good \
         code\" and not \"did the subagent do what it said it would do\"; it is \
         \"does this change do what the brief asked for, without breaking what was \
         already there\".\n\
         4. **Hand the verdict in with `review_verdict`.** Your verdict is not prose and \
         it is not a block at the end of your reply: call `review_verdict` ONCE, when you \
         have judged, with `verdict` one of `accept`, `reject` or `needs_human`, and with \
         `reasons`, `files` and `commands` — the reasons in your own words, and the files \
         you read and the commands you ran against the branch and base above, because a \
         verdict against a SHA nobody can name is not evidence. **That call is the whole \
         of your answer to the queue**: the row comes off its board because the verdict \
         reached it, and a reply that ends in prose without that call has judged nothing — \
         the entry waits and this attempt is counted against it. Explain what you found \
         in your reply too — the session that started you reads it there — and keep to \
         the evidence.\n\
         5. **One command per call.** Every `bash` call runs exactly one simple \
         command: no `&&`, no `;`, no `|`, no `>` or `<` to a file, no `$(…)` or \
         backticks, and no `cd`-then-command. The permission rules are tested one \
         simple command at a time — a substitution, a redirection to a file and a group \
         are never matched — so a compound command is a card for a person, and a review \
         that uses them cannot run unattended: with nobody at the keyboard the call is \
         not run at all, the attempt fails, and the queue spends one of the three attempts \
         it has.\n\n\
         {shapes_head}\n\
         git diff {base}...{head}\n\
         git diff --stat {base}...{head}\n\
         git log --oneline -5 {base}..{head}\n\
         git show SHA\n\
         rg PATTERN PATH\n\
         cargo test -p CRATE\n\n\
         Two things that are not exceptions. When you need another directory, pass it to \
         the tool — `bash` takes a `cwd`, and `read`, `grep` and `glob` take a path — \
         rather than `cd`-ing to it first. When you want a shorter output, ask for a \
         narrower command (`git diff --stat`, `git log --oneline -5`) rather than piping \
         it into `head`. And the `commands` field of `review_verdict` is a LIST and not a \
         shell line: each item in it is ONE simple command, written the way you ran it, \
         and the list itself is the separator — nothing in it needs a `;`.\n\n\
         6. **Read the references, and say which.** When the change leans on an API, a \
         command's flags, a protocol, a file format or a spec, check the claim against the \
         source rather than against the code or your memory of it: `man CMD` and `CMD --help` \
         for a tool, the crate's or library's own docs and source for an API, and — when the \
         session seats them — `web_search` and `web_fetch` for upstream docs, RFCs, release \
         notes and the issue a change cites. You are encouraged to: a verdict that checked a \
         flag in its man page is worth more than one that trusted the diff. Name what you read \
         in `files` (a URL counts) and what it told you in `reasons`.",
        brief = req.brief,
        branch = req.branch,
        base = req.base_sha,
        head = head,
        cwd = cwd,
        shapes_head = ONE_COMMAND_SHAPES_HEAD,
    )
}

/// The line that opens the block of example commands in [`review_prompt`], and the anchor
/// the tests parse that block back out by: one constant, so the writer and the reader cannot
/// disagree about where the list begins.
const ONE_COMMAND_SHAPES_HEAD: &str = "The shapes a review needs, one per line, and each line is ONE command with nothing \
     after it:";

/// The seam the merge queue lands on: the queue hands an entry's brief and placement facts
/// here, and a gatekeeper session is asked with [`review_prompt`].
///
/// A request with an empty entry id, brief, branch or base is refused BY NAME rather than
/// handed on: a reviewer given no ask reviews the artifact against nothing, and a verdict
/// against a SHA nobody can resolve is not evidence.
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

/// The door a wake goes through: the daemon's two acts, and nothing else.
///
/// The queue's thread owns no session, so the ask is a fact written down plus a bell rung and
/// the answer returns through the store on a later pass. That keeps a minutes-long review from
/// blocking a serial queue; a door that can do neither returns `Err`, never a wake that was not.
pub trait Reviewer {
    /// Write the request down and ring the reviewer's bell. `Ok` is what was done, in the
    /// words the operator gets; `Err` is why it could not be, said rather than swallowed.
    fn wake(&self, entry_id: &str, req: &ReviewRequest) -> Result<String, String>;

    /// Whether a session exists that will judge an entry's branch: `Ok(())` is *one will*,
    /// `Err` is why none will.
    ///
    /// The queue asks this before it asks for a verdict, because *nobody has asked yet* and
    /// *nobody is there to ask* are two different facts only the door can tell apart. The
    /// default `Ok(())` is a door with no registry, which must not read as *there is none*.
    fn gatekeeper_ready(&self, _entry_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// Tell the project's agent that the entry's repository has no merge gate, once, when an
    /// entry starts waiting for one. The default tells nobody, which is a door with no
    /// sessions behind it.
    fn gate_missing(&self, _entry_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// Tell the project's agent that no gatekeeper can judge its branch: the queue's fallback,
    /// through the session the branch came from, which owns the worktree it is checked out in.
    ///
    /// The interface between the two halves is a branch and not a session: the gatekeeper's
    /// contribution is a verdict, and the gate and the merge are git and a shell a project can
    /// run itself. `why` is the queue's own words, passed rather than re-derived.
    ///
    /// `Ok` is what was done and `Err` is why it could not be: *the project was told* and
    /// *there was nobody to tell* are two facts a person acts differently on.
    fn no_gatekeeper(&self, entry_id: &str, _why: &str) -> Result<String, String> {
        Err(format!(
            "the door this daemon was built with has no session to tell about `{entry_id}`"
        ))
    }
}

/// A door that refuses by name: what a build with no reviewer session attached answers.
///
/// An entry waiting for a verdict nobody was asked for waits for ever, so *nothing was asked*
/// is said out loud rather than reported as a wake.
pub struct NoReviewer;

impl Reviewer for NoReviewer {
    fn wake(&self, entry_id: &str, req: &ReviewRequest) -> Result<String, String> {
        Err(format!(
            "this daemon has no gatekeeper session attached, so nothing was asked about \
             `{entry_id}` (branch `{}`, base `{}`). The branch does not land without a verdict.",
            req.branch, req.base_sha
        ))
    }

    /// And this door has nobody to tell either: a build with no reviewer session has no
    /// registry behind it, so the fallback cannot reach a project session from here.
    fn no_gatekeeper(&self, entry_id: &str, _why: &str) -> Result<String, String> {
        Err(format!(
            "this daemon has no session to tell about `{entry_id}`, and no gatekeeper session \
             attached either. The branch does not land without a verdict."
        ))
    }
}

/// The value on a line that begins with `label:`, or `None`. Case-insensitive on the label
/// and anchored at the start of the line, so a mention of the word in prose is not read as
/// the field.
///
/// Shared with the notes keeper ([`crate::notes_keeper::parse_report`]), which reads a block
/// of exactly this shape: one reader for one shape, or the two drift the first time either
/// grows a case.
pub(crate) fn label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let t = line.trim_start();
    let (head, tail) = t.split_once(':')?;
    head.trim().eq_ignore_ascii_case(label).then(|| tail.trim())
}

/// One field's worth of bullets: a single line carrying several `- ` items, or one, and
/// `none` means nothing, on [`commas`]' rule. Shared with [`crate::notes_keeper`].
pub(crate) fn bullets(v: &str) -> Vec<String> {
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

/// A comma-separated list, with `none` meaning nothing: the prompt names it as the way to
/// say *I looked at nothing*, and it must not become a file called `none`.
pub(crate) fn commas(v: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ReviewRequest {
        ReviewRequest {
            brief: "add a gatekeeper seat and a brief-first protocol".into(),
            branch: "agent/gatekeeper".into(),
            base_sha: "a4fbe70".into(),
            head_sha: String::new(),
            cwd: String::new(),
        }
    }

    /// The brief is the only framing and it is there verbatim, the placement is named, and
    /// the prompt says the report is not given. The report's absence as data is structural, so
    /// this asserts the stated negative rather than searching for a field that cannot exist.
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

    /// The prompt names the door the verdict goes through, and its three words: the tool's
    /// name, the closed set and the fields the call takes. A reviewer told none of them holds
    /// the door and no way to know it is the answer.
    #[test]
    fn the_prompt_names_the_door_the_verdict_goes_through() {
        let p = review_prompt(&request());
        assert!(
            p.contains("review_verdict"),
            "the reviewer must be told the tool its verdict goes through:\n{p}"
        );
        for word in ["`accept`", "`reject`", "`needs_human`"] {
            assert!(p.contains(word), "the prompt must name `{word}`:\n{p}");
        }
        for field in ["`reasons`", "`files`", "`commands`"] {
            assert!(p.contains(field), "the prompt must name `{field}`:\n{p}");
        }
        assert!(
            p.contains("has judged nothing"),
            "and what a reply that never made the call means:\n{p}"
        );
        // And the block is GONE: a second spelling of the answer is a second answer, and a
        // prompt that still asked for it would be asking a reviewer to judge nothing.
        for gone in ["verdict:", "reasons:", "files:", "commands:"] {
            assert!(
                !p.contains(gone),
                "the prompt still asks for the closing block (`{gone}`), which nothing reads \
                 any more:\n{p}"
            );
        }
    }

    /// The rule that decides whether a review can run at all, in the prompt's own words: one
    /// simple command per call, every shape that breaks it, and the reason. Asserted as strings
    /// on the rendered prompt, so the demand cannot be dropped in an edit that leaves the rest.
    #[test]
    fn the_prompt_demands_one_simple_command_per_call_and_says_why() {
        let p = review_prompt(&request());
        for want in [
            "One command per call",
            "no `&&`",
            "no `;`",
            "no `|`",
            "no `>` or `<` to a file",
            "no `$(…)` or backticks",
            "no `cd`-then-command",
            "a card for a person",
            "cannot run unattended",
        ] {
            assert!(p.contains(want), "the prompt must state `{want}`:\n{p}");
        }
        // The two substitutions a reviewer reaches for a compound command to get, each named
        // where it would look for it.
        assert!(p.contains("`bash` takes a `cwd`"), "{p}");
        assert!(p.contains("rather than piping it into `head`"), "{p}");
        // And the `commands` field is named as a LIST, so the rule above cannot be read as
        // forbidding the evidence field beside it: its items are already the separator.
        assert!(p.contains("is a LIST and not a shell line"), "{p}");
    }

    /// The command shapes the prompt states, read back out of the prompt's own text: the
    /// non-empty lines after `ONE_COMMAND_SHAPES_HEAD`.
    ///
    /// Parsed rather than taken from a constant, because a test that read the writer's own list
    /// would pass while the prompt said something else.
    fn stated_command_shapes(prompt: &str) -> Vec<String> {
        let mut lines = prompt
            .lines()
            .skip_while(|l| l.trim() != ONE_COMMAND_SHAPES_HEAD);
        lines.next(); // the head itself
        lines
            .map(str::trim)
            .take_while(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Every command the prompt states is one simple command, and the seed rules admit it.
    ///
    /// The matcher refuses to split a substitution, a redirection, a group and a `&&`/`;`/`|`,
    /// so one in an example teaches the very card the rule forbids, and an example the seed
    /// does not preapprove costs a person a card.
    #[test]
    fn every_shape_the_prompt_states_is_one_simple_command_and_costs_no_card() {
        let shapes = stated_command_shapes(&review_prompt(&request()));
        assert!(
            shapes.len() >= 4,
            "the prompt must state the shapes a review needs, one per line: {shapes:?}"
        );
        for shape in &shapes {
            for bad in ["&&", ";", "|", ">", "<", "$(", "`"] {
                assert!(
                    !shape.contains(bad),
                    "the prompt states `{shape}`, which carries `{bad}` — a compound command \
                     is a card for a person. The stated shapes were: {shapes:?}"
                );
            }
        }
        let seed = crate::permission::seed();
        for shape in &shapes {
            assert_eq!(
                crate::permission::evaluate_bash(shape, &[&seed]),
                crate::permission::Action::Allow,
                "`{shape}` is not preapproved by the seed, so a review that runs it is a \
                 permission card — which is the defect this prompt rule exists to close"
            );
        }
    }

    /// The closed set is closed: one answer per word and `None` for everything else, because
    /// reading an unknown word as `accept` would land a branch nobody accepted.
    #[test]
    fn the_decision_words_are_a_closed_set() {
        for d in Decision::ALL {
            assert_eq!(Decision::parse(d.as_str()), Some(d));
        }
        assert_eq!(Decision::ALL.len(), 3, "the set has one word per decision");
        assert_eq!(Decision::parse("Accept"), None, "the words are exact");
        assert_eq!(Decision::parse("maybe"), None);
        assert_eq!(Decision::parse(""), None);
    }

    /// The wake hands the request to the door and reports what the door did, and it refuses a
    /// request nobody could act on before the door is reached.
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

        // A blank ask never reaches the door: three ways to be unactionable, each refused with
        // the field it is about rather than handed to a reviewer who would review the artifact
        // against nothing.
        let blank = ReviewRequest {
            brief: "  ".into(),
            branch: "agent/x".into(),
            base_sha: "abc".into(),
            head_sha: String::new(),
            cwd: String::new(),
        };
        let e = wake("m-2", blank, &door).unwrap_err();
        assert!(e.contains("no brief"), "{e}");
        let nameless = ReviewRequest {
            brief: "do the thing".into(),
            branch: "".into(),
            base_sha: "abc".into(),
            head_sha: String::new(),
            cwd: String::new(),
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

        // And a build with no reviewer says so rather than reporting a wake.
        let e = wake("m-4", request(), &NoReviewer).unwrap_err();
        assert!(e.contains("no gatekeeper session"), "{e}");
        assert!(e.contains("does not land without a verdict"), "{e}");
    }
}

/// The heading the merge gate lives under, in the repository's `AGENTS.md`.
///
/// A section rather than a file of its own because that file is already the one agents and
/// people both read about how a project is worked on.
pub const MERGE_GATE_HEADING: &str = "Merge gate";

/// The gate's steps, out of an `AGENTS.md`: the first fenced block under a `Merge gate`
/// heading (any level, any case), one command per line, blank lines and `#` skipped. `None`
/// for no such section or no command in its block: a gate nobody wrote passes nothing.
///
/// The block ends at its closing fence, and the search ends at the next heading of the same
/// or a higher level, so a fence further down the file cannot be read as this one.
pub fn parse_merge_gate(md: &str) -> Option<Vec<String>> {
    let heading = |line: &str| -> Option<(usize, String)> {
        let t = line.trim_start();
        let level = t.chars().take_while(|c| *c == '#').count();
        (level > 0 && t[level..].starts_with(' ')).then(|| {
            (
                level,
                t[level..].trim().trim_end_matches('#').trim().to_string(),
            )
        })
    };
    let mut lines = md.lines();
    let level = loop {
        let line = lines.next()?;
        if let Some((level, title)) = heading(line)
            && title.eq_ignore_ascii_case(MERGE_GATE_HEADING)
        {
            break level;
        }
    };
    let mut steps = Vec::new();
    let mut fence: Option<&str> = None;
    for line in lines {
        let t = line.trim();
        match fence {
            None => {
                if let Some((l, _)) = heading(line)
                    && l <= level
                {
                    return None;
                }
                if t.starts_with("```") {
                    fence = Some("```");
                } else if t.starts_with("~~~") {
                    fence = Some("~~~");
                }
            }
            Some(f) => {
                if t.starts_with(f) {
                    break;
                }
                if !t.is_empty() && !t.starts_with('#') {
                    steps.push(t.to_string());
                }
            }
        }
    }
    (!steps.is_empty()).then_some(steps)
}

/// The section, written: what [`parse_merge_gate`] reads back, for an agent to put into
/// `AGENTS.md` once the operator has chosen.
pub fn merge_gate_section(steps: &[String]) -> String {
    format!(
        "## {MERGE_GATE_HEADING}\n\n\
         The merge queue runs these in a branch's worktree, after rebasing it onto `main` and \
         before landing it; the first that fails parks the branch. Read from `main`'s copy of \
         this file, so a branch cannot change its own gate.\n\n\
         ```sh\n{}\n```\n",
        steps.join("\n")
    )
}

#[cfg(test)]
mod merge_gate_section {
    use super::*;

    #[test]
    fn the_gate_is_the_first_block_under_its_heading() {
        let md = "# Project\n\nSome text.\n\n```sh\nnot this\n```\n\n## Merge gate\n\nWhy.\n\n\
                  ```sh\n# fmt first\nmake fmt-check\n\nmake test\n```\n\n```sh\nnor this\n```\n";
        assert_eq!(
            parse_merge_gate(md),
            Some(vec!["make fmt-check".into(), "make test".into()])
        );
        // Written and read back.
        let steps = vec!["cargo test --workspace".to_string()];
        assert_eq!(parse_merge_gate(&merge_gate_section(&steps)), Some(steps));
    }

    #[test]
    fn no_section_or_an_empty_block_is_no_gate() {
        assert_eq!(
            parse_merge_gate("# Project\n\n```sh\nmake test\n```\n"),
            None
        );
        assert_eq!(
            parse_merge_gate("## Merge gate\n\n```sh\n# nothing\n```\n"),
            None
        );
        // A block past the next heading is another section's.
        assert_eq!(
            parse_merge_gate("## Merge gate\n\nTBD\n\n## Other\n\n```sh\nmake test\n```\n"),
            None
        );
        // Any level, any case, tildes too.
        assert_eq!(
            parse_merge_gate("### merge GATE\n~~~\ngo test ./...\n~~~\n"),
            Some(vec!["go test ./...".into()])
        );
    }
}
