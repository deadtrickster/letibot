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
//! The gatekeeper's own half of the merge queue: the seat
//! ([`crate::runtime::Role::gatekeeper`]), the brief-first request ([`ReviewRequest`]),
//! the verdict ([`Verdict`]), the prompt ([`review_prompt`]) — and the two doors, which
//! face opposite ways.
//!
//! # The two doors, and which way each one faces
//!
//! [`wake`] is the queue's door TO a reviewer: the daemon writes the ask down and rings
//! the session that hosts the review, and [`Reviewer`] is the daemon's own half of that.
//! The reviewer's door is the other way, and it is the one the reviewer CALLS:
//! `review_verdict` ([`crate::builtins::verdict::ReviewVerdictTool`]), carrying a
//! [`Submission`] to the desk the daemon holds
//! ([`crate::builtins::verdict::ReviewDesk`]), from which the daemon writes the entry's
//! review row through the same function it always did.
//!
//! **The answer is the child's own act and not something read out of its prose.** The
//! closing block this prompt used to ask for is gone, and so is the parser that read it:
//! re-parsing prose to recover a label is how a corpus rots, and the label here decides
//! whether a branch lands. A reviewer that answers in prose has judged nothing, and the
//! entry waits — the safe direction, and the one the daemon's own pass reports.
//!
//! # What this module deliberately does not do
//!
//! - **No queue.** The merge queue is its own module in the daemon; this is the half it
//!   wakes and the half a reviewer answers into.
//! - **No storage.** There is no table of entries here. An entry is the queue's;
//!   this module takes one, as a [`ReviewRequest`], and says what a reviewer asked
//!   with it would be told. The desk that holds the handed-in verdicts is not a second
//!   store either: what it holds is written onto the queue's own row by the queue's own
//!   daemon, in the same pass that already owns that write.

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
    /// The base SHA the branch was cut from. The diff is `base...head`, and the
    /// verdict names it for the same reason.
    pub base_sha: String,
    /// **The head under review** (R61 decision 6: the verdict is bound to the head it judged).
    /// The diff the reviewer reads is `base...head_sha`, not `base...branch`, so a commit made
    /// on the branch while the review runs is not part of what was judged — the queue lands only
    /// this sha. Empty is *the branch as it is* (a request that predates the binding).
    pub head_sha: String,
    /// **Where to run the review's commands**: the repository on the host, or empty for the
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

/// **What a reviewer HANDS IN** — the verdict as the child's own act, and not as prose somebody
/// else read.
///
/// The three fields are the whole of what a reviewer may say, and **there is no field for the
/// branch or the base**: those are the [`ReviewRequest`]'s, joined to this at the daemon's write
/// ([`Verdict::for_request`]), so a verdict can never name a SHA the queue did not hand over —
/// the same structural argument [`ReviewRequest`] makes about the child's report, one direction
/// over.
///
/// It is what `review_verdict` carries
/// ([`crate::builtins::verdict::ReviewVerdictTool`]), and the only thing that becomes a row's
/// `decision`. A reply in prose is not one of these and is never read as one.
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

/// The exact text the gatekeeper is asked with: the brief, the placement, and the
/// rules.
///
/// The rules are the protocol in the reviewer's own prompt: read the code, do not
/// fix it, judge the artifact against the ask, say what you looked at, and **hand the
/// verdict in through the one door a reviewer holds**. The brief is the only framing, and
/// it is there verbatim; the placement names the branch and base the verdict is joined to.
///
/// # The verdict is a TOOL CALL, and that is the second change here
///
/// This prompt has asked two different things of a model. First *"End with your verdict:
/// accept, reject, or needs_human — the reasons, and what you looked at"*, which asks for
/// prose and then leaves whoever reads it to decide what the prose meant. Then a closing
/// block (`verdict:`/`reasons:`/`files:`/`commands:`) read by a parser — better, and still
/// a label recovered from a model's text by a reader that has to guess where the block
/// starts, and still exactly the defect this tree's own store warns about: *"re-parsing
/// prose to recover a label is how a corpus rots"*. It is worse here than in a corpus,
/// because the label the queue reads is the one that decides whether a branch lands.
///
/// So the verdict is **`review_verdict`**
/// ([`crate::builtins::verdict::ReviewVerdictTool`]): the reviewer calls it once, with the
/// decision and its reasons as arguments, and the call IS the answer. There is no block to
/// find, no last-line rule, and no prose for anybody to read a decision out of — a reviewer
/// that answers in prose has judged nothing, and the entry waits (`Harness::review_outcome`
/// counts that attempt rather than inventing a verdict from it).
///
/// # One command per call, and why the prompt is where that is said
///
/// A gatekeeper inspects a branch with `cd X && git diff … | head -50`, `… > /tmp/out`,
/// `$(git rev-parse …)` — compound commands — and **every one of them is a permission
/// card, by construction.** The rules are tested one simple command at a time: a
/// substitution, a redirection to a file and a group are never matched at all, so the
/// shape falls outside every preapproval however the operator's file is written. The cost
/// is two-sided, and both sides were measured on this box: with a person at the keyboard
/// nineteen queued reviews drown the head (*"i got drown in permission prompts"*), and
/// with nobody there each call is `not run`, the attempt fails, and the queue spends one
/// of its three against a branch nobody judged.
///
/// So the prompt demands one simple command per call, states the reason, and gives the
/// two substitutions that replace the shapes a review actually reaches for: `cwd` (which
/// `bash` takes) instead of `cd`-then-command, and a narrower command instead of `| head`.
///
/// **And the prompt is the only half that can carry it today.** The seat cannot refuse the
/// shape: a [`crate::runtime::Role`] is a name, a list of tool names and a ceiling, and
/// nothing in the exec path — `InvokeCtx`, `GateCall`, `AdjudicatedGate` — is told which
/// seat a call came from, so a refusal at the seat would mean plumbing the seat into the
/// gate first. The prompt is the load-bearing half; this is the honest note about why it is
/// currently the whole of it.
///
/// The example commands are a block the tests parse back out of the rendered prompt
/// (`ONE_COMMAND_SHAPES_HEAD` anchors it) rather than a list compared against a constant,
/// because a test that read the writer's own list would pass while the prompt said
/// something else. The placeholders are capitals (`SHA`, `CRATE`) and not `<sha>`: `<` is a
/// redirection to a shell, so a shape written that way would teach the very thing this rule
/// forbids.
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
         the entry waits and this attempt is counted against it. You may reason in prose \
         for as long as you like before and after the call, and what you found belongs in \
         your reply too: the session that started you reads it there.\n\
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
         and the list itself is the separator — nothing in it needs a `;`.",
        brief = req.brief,
        branch = req.branch,
        base = req.base_sha,
        head = head,
        cwd = cwd,
        shapes_head = ONE_COMMAND_SHAPES_HEAD,
    )
}

/// **The line that opens the block of example commands in [`review_prompt`]** — the anchor
/// the tests parse that block back out by, and one constant so the writer and the reader
/// cannot disagree about where the list begins.
const ONE_COMMAND_SHAPES_HEAD: &str = "The shapes a review needs, one per line, and each line is ONE command with nothing \
     after it:";

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

    /// **Whether a session exists that will judge an entry's branch** — `Ok(())` is *one will*,
    /// and `Err` is why none will: the never-started case of the queue's fallback.
    ///
    /// The queue asks this BEFORE it asks for a verdict, and the difference it makes is the whole
    /// of the operator's argument for a fallback: *"even if gatekeeper session dies or didnt
    /// start - the queue tooling can send a message back to the main project session and projects
    /// will gate and merge on their own"*. *Nobody has asked yet* and *nobody is there to ask* are
    /// two different facts, and only the door can tell them apart — it is the one holding the
    /// registry and the store. The daemon's own door carries the grace that draws the line
    /// between *not open yet* and *never started* (`GatekeeperDoor::gatekeeper_ready`), because a
    /// session the daemon's own clock opens on its first idle pass is not a missing gatekeeper.
    ///
    /// The default says `Ok(())`, which is a door with no registry behind it (and every test's):
    /// a door that cannot answer *is there a gatekeeper* must not be read as *there is none*, or
    /// every daemon built with one would hand its entries to their projects on the first pass.
    fn gatekeeper_ready(&self, _entry_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// **Tell the project's agent that the entry's repository has no merge gate.** Called once,
    /// when an entry starts waiting for one. The default tells nobody, which is a door with no
    /// sessions behind it (and every test's); the daemon's door rings the session that queued it.
    fn gate_missing(&self, _entry_id: &str) -> Result<(), String> {
        Ok(())
    }

    /// **Tell the project's agent that no gatekeeper can judge its branch** — the queue's
    /// fallback, and the door it goes through: the session the branch came from, which owns the
    /// worktree the branch is checked out in.
    ///
    /// The operator's argument for it, verbatim: *"even if gatekeeper session dies or didnt start
    /// - the queue tooling can send a message back to the main project session and projects will
    /// gate and merge on their own"*, *"because their code lives inside them in worktrees
    /// anyway"*. So the interface between the two halves is a BRANCH and not a session: the
    /// gatekeeper's contribution is a verdict, and the gate and the merge are git and a shell —
    /// which a project can run itself, from the worktree it already has.
    ///
    /// `why` is the queue's own words for what happened (never started, died, or an ask that
    /// failed), and it is passed rather than re-derived: the door does not read the queue's
    /// vocabulary and the queue does not guess at the door's.
    ///
    /// `Ok` is what was done, in the words the operator gets; `Err` is why it could not be, said
    /// rather than swallowed — the entry's row carries it, because *the project was told* and
    /// *there was nobody to tell* are the two facts a person acts differently on. The default
    /// tells nobody and says so, which is a door with no sessions behind it.
    fn no_gatekeeper(&self, entry_id: &str, _why: &str) -> Result<String, String> {
        Err(format!(
            "the door this daemon was built with has no session to tell about `{entry_id}`"
        ))
    }
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

    /// **And this is the door that has nobody to tell either.** A build with no reviewer session
    /// is a build with no registry behind the door, so the queue's fallback cannot reach a project
    /// session from here: the entry's row says *nobody could be told*, and the person's verbs are
    /// what is left. [`Reviewer::no_gatekeeper`]'s default says the same thing one word wider; this
    /// is that sentence for the door whose whole reason for existing is that it has no sessions.
    fn no_gatekeeper(&self, entry_id: &str, _why: &str) -> Result<String, String> {
        Err(format!(
            "this daemon has no session to tell about `{entry_id}`, and no gatekeeper session \
             attached either. The branch does not land without a verdict."
        ))
    }
}

/// The value on a line that begins with `label:`, or `None`. Case-insensitive on the label and
/// anchored at the start of the line, so a mention of the word `verdict` in the reviewer's own
/// prose is not mistaken for the field.
///
/// **Shared with the notes keeper** ([`crate::notes_keeper::parse_report`]), which reads a
/// closing block of exactly this shape. One reader for one shape: a second implementation one
/// module over would be a second answer to *is this line the label*, and the two would drift the
/// first time either grew a case.
pub(crate) fn label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let t = line.trim_start();
    let (head, tail) = t.split_once(':')?;
    head.trim().eq_ignore_ascii_case(label).then(|| tail.trim())
}

/// One field's worth of bullets: a single line that carries several `- ` items, or one — and
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

/// A comma-separated list, with `none` meaning nothing — which the prompt names as the way to
/// say *I looked at nothing*, and which must not become a file called `none`. Shared with
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

    /// **The prompt names the door the verdict goes through, and its three words.** The verdict
    /// is a tool call now — see `review_prompt`'s own doc for why prose and then a closing block
    /// were both replaced — so what has to be in the text is the tool's name, the closed set and
    /// the fields the call takes. A prompt that named none of them would leave a reviewer with
    /// the only door it holds and no way to know it is the answer.
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
        // **And the block is GONE**, because a second spelling of the answer is a second answer:
        // a reviewer that wrote the old closing block instead of calling the tool would have
        // judged nothing, and a prompt that still asked for it would be asking for that.
        for gone in ["verdict:", "reasons:", "files:", "commands:"] {
            assert!(
                !p.contains(gone),
                "the prompt still asks for the closing block (`{gone}`), which nothing reads \
                 any more:\n{p}"
            );
        }
    }

    /// **The prompt states the rule that decides whether a review can run at all**, in its own
    /// words: one simple command per call, every shape that breaks it, and the reason. Asserted
    /// as strings on the rendered prompt, so the demand cannot be dropped in an edit that leaves
    /// the closing block — which every other test here would still pass.
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
        // **And the `commands` list is named as a list**, so the rule above cannot be read as
        // forbidding the evidence field it sits beside: the field is a list of simple commands
        // and needs no separator, which is the one `;` a reader might have expected.
        assert!(p.contains("is a LIST and not a shell line"), "{p}");
    }

    /// The command shapes the prompt states, **read back out of the prompt's own text**: the
    /// non-empty lines that follow `ONE_COMMAND_SHAPES_HEAD`, trimmed.
    ///
    /// Parsed rather than taken from a constant, because a test that read the writer's own list
    /// would pass while the prompt said something else — which is the one thing this pair of
    /// tests exists to catch.
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

    /// **Every command the prompt states is one simple command, and the seed rules admit it.**
    ///
    /// The first half is the shape the permission matcher can read at all: a substitution, a
    /// redirection to a file, a group and a `&&`/`;`/`|` are exactly what it refuses to split,
    /// so one of them in an example is the prompt teaching the card it forbids. `<` and `>` are
    /// in the list for that reason and not for tidiness — which is why the placeholders are
    /// capitals and not `<sha>`.
    ///
    /// The second half is the consequence, and it is the defect itself: an example the seed does
    /// not preapprove is an example that costs a person a card. The seed is the floor this
    /// repository installs; the operator's own file outranks it (last match wins), and a rule
    /// they deleted is their call rather than this test's.
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

    /// **The closed set is closed, and one word out is not a verdict.** The parse is the same
    /// question the door asks (`ReviewDesk::submit` refuses a word outside it), and reading an
    /// unknown word as `accept` would land a branch nobody accepted — so there is one answer per
    /// word and `None` for everything else.
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

        // **And a build with no reviewer says so rather than reporting a wake.**
        let e = wake("m-4", request(), &NoReviewer).unwrap_err();
        assert!(e.contains("no gatekeeper session"), "{e}");
        assert!(e.contains("does not land without a verdict"), "{e}");
    }
}

/// **The heading the merge gate lives under**, in the repository's `AGENTS.md`.
///
/// The operator, 2026-10-09: *"make the gate configurable per repo … a project has this gate
/// command, which must go to repo obviously"*, and *"then it becomes a section in agents.md for
/// example and can be parsed from it"*. A section rather than a file of its own because the
/// file is already the one agents and people both read about how a project is worked on.
pub const MERGE_GATE_HEADING: &str = "Merge gate";

/// **The gate's steps, out of an `AGENTS.md`**: the first fenced block under a `Merge gate`
/// heading (any level, any case), one command per line; blank lines and `#` comments skipped.
/// `None` when there is no such section, or its block has no command in it — a gate nobody
/// wrote is not a gate that passes everything.
///
/// The block ends at its closing fence, and the search for it ends at the next heading of the
/// same or a higher level, so a fence further down the file cannot be read as this one.
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

/// **The section, written** — what [`parse_merge_gate`] reads back, for an agent to put into
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
