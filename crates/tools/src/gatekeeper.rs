//! The gatekeeper: the reviewer that judges a subagent's work before it can land.
//! It is given the child's brief and never its report, and it answers with a
//! `review_verdict` call rather than with prose somebody reads a label out of.

/// The request the gatekeeper is given: the original brief, verbatim, and the
/// placement facts. There is no field for a report, so a report cannot be smuggled
/// in, and the prompt is a function of exactly these fields and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    /// The prompt the child was spawned with, verbatim. The only framing the
    /// reviewer is allowed to start from.
    pub brief: String,
    /// The branch the work landed on. Named in the verdict, because a verdict
    /// against a branch nobody can name is not evidence.
    pub branch: String,
    /// The base SHA the branch was cut from. The diff is `base...head`, and the
    /// verdict names it for the same reason.
    pub base_sha: String,
    /// The head under review: the diff read is `base...head_sha`, not `base...branch`, so a
    /// commit made while the review runs is not part of what was judged. Empty means the
    /// branch as it is.
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

    /// Every decision, in the order the queue reads them. An `Accept` is the only one that
    /// lands and the other two are parked for a person, which is why `Accept` is first and
    /// the two refusals follow in the order a reviewer is likelier to mean them.
    pub const ALL: [Decision; 3] = [Decision::Accept, Decision::Reject, Decision::NeedsHuman];

    /// The decision a stored word names, if it names one. `None` for a word outside the set,
    /// and that is a refusal rather than a default: reading an unknown word as `Accept` would
    /// land a branch on the strength of a typo.
    pub fn parse(stored: &str) -> Option<Decision> {
        Self::ALL.into_iter().find(|d| d.as_str() == stored)
    }
}

/// What the reviewer actually looked at: the files it read and the commands it ran. A
/// verdict that names no files and runs no commands is an opinion, and this is the field
/// that keeps the two apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LookedAt {
    /// The files the reviewer read.
    pub files: Vec<String>,
    /// The commands the reviewer ran: `git diff`, `git log`, and the like.
    pub commands: Vec<String>,
}

/// The verdict: the entry judged, the decision, the reasons, and the evidence. The branch
/// and base come from the [`ReviewRequest`] the reviewer was asked about, so a verdict can
/// never name a SHA the queue did not.
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
    /// A verdict against the request it was asked about, so it can never name a SHA the
    /// queue did not.
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

/// What a reviewer hands in: the verdict as the child's own act, not prose somebody else read.
/// There is no field for the branch or the base; those are the [`ReviewRequest`]'s, joined to
/// this at the daemon's write, so a verdict can never name a SHA the queue did not hand over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// One word out of [`Decision::ALL`], and nothing else: a word outside the set is refused
    /// by the door rather than defaulted, because guessing `Accept` lands a branch nobody
    /// accepted and guessing `Reject` parks work nobody rejected.
    pub decision: Decision,
    /// Why, in the reviewer's own words.
    pub reasons: Vec<String>,
    /// What it actually looked at.
    pub looked_at: LookedAt,
}

/// The exact text the gatekeeper is asked with: the brief, the placement, and the rules. The
/// brief is the only framing and it is there verbatim; the verdict is the `review_verdict`
/// call, and one simple command per call is the prompt's half of a rule the seat cannot refuse.
pub fn review_prompt(req: &ReviewRequest) -> String {
    // The revision the diffs name: the head under review when the queue bound one, else the
    // branch (see `ReviewRequest::head_sha`).
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

/// The line that opens the block of example commands in [`review_prompt`]: the anchor the
/// tests parse that block back out by, so the writer and the reader cannot disagree about
/// where the list begins.
const ONE_COMMAND_SHAPES_HEAD: &str = "The shapes a review needs, one per line, and each line is ONE command with nothing \
     after it:";

/// The value on a line that begins with `label:`, or `None`. Case-insensitive on the label and
/// anchored at the start of the line, so a mention of the word in the reviewer's prose is not
/// the field. Shared with [`crate::notes_keeper::parse_report`], one reader for one shape.
pub(crate) fn label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let t = line.trim_start();
    let (head, tail) = t.split_once(':')?;
    head.trim().eq_ignore_ascii_case(label).then(|| tail.trim())
}

/// One field's worth of bullets: a single line that carries several `- ` items, or one, and
/// `none` means nothing on [`commas`]' rule. Shared with [`crate::notes_keeper`].
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

/// A comma-separated list, with `none` meaning nothing, which the prompt names as the way to
/// say I looked at nothing and which must not become a file called `none`. Shared with
/// [`crate::notes_keeper`].
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

    /// The brief is the only framing, and it is there verbatim; the placement is named; and the
    /// prompt says the report is not given. The request has no field for a report, so the test
    /// asserts the stated negative rather than searching for one.
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

    /// The prompt names the door the verdict goes through, and its three words: the tool's name,
    /// the closed set and the fields the call takes. A prompt naming none of them would leave a
    /// reviewer with the only door it holds and no way to know it is the answer.
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
        // The block is gone, because a second spelling of the answer is a second answer: a
        // reviewer that wrote the old block instead of calling the tool would have judged
        // nothing, and a prompt that still asked for it would be asking for that.
        for gone in ["verdict:", "reasons:", "files:", "commands:"] {
            assert!(
                !p.contains(gone),
                "the prompt still asks for the closing block (`{gone}`), which nothing reads \
                 any more:\n{p}"
            );
        }
    }

    /// The prompt states the rule that decides whether a review can run at all: one simple
    /// command per call, every shape that breaks it, and the reason. Asserted as strings on the
    /// rendered prompt, so the demand cannot be dropped in an edit.
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
        // And the `commands` list is named as a list, so the rule above cannot be read as
        // forbidding the evidence field it sits beside: the field is a list of simple commands
        // and needs no separator, which is the one `;` a reader might have expected.
        assert!(p.contains("is a LIST and not a shell line"), "{p}");
    }

    /// The command shapes the prompt states, read back out of the prompt's own text: the
    /// non-empty lines that follow `ONE_COMMAND_SHAPES_HEAD`, trimmed. Parsed rather than taken
    /// from a constant, since a test that read the writer's own list would pass on anything.
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

    /// Every command the prompt states is one simple command, and the seed rules admit it. A
    /// substitution, a redirection, a group or a `&&`/`;`/`|` is exactly what the matcher refuses
    /// to split, and an example the seed does not preapprove costs a person a card.
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

    /// The closed set is closed, and one word out is not a verdict. Reading an unknown word as
    /// `accept` would land a branch nobody accepted, so there is one answer per word and `None`
    /// for everything else.
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
}

/// The heading the merge gate lives under, in the repository's `AGENTS.md`. A section rather
/// than a file of its own, because that file is already the one agents and people both read
/// about how a project is worked on.
pub const MERGE_GATE_HEADING: &str = "Merge gate";

/// The gate's steps, out of an `AGENTS.md`: the first fenced block under a `Merge gate` heading
/// (any level, any case), one command per line; blank lines and `#` comments skipped. The block
/// ends at its closing fence and the search at the next heading of the same level.
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

/// The section, written, for what [`parse_merge_gate`] reads back, for an agent to put into
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
