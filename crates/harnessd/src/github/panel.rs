//! The merge queue's review panel, on a GitHub pull request: the members judge, the gatekeeper
//! decides, and one GitHub review comes out of the verdicts (R118's third piece).
//!
//! A member is any reviewer that answers the queue's [`Verdict`] - R116's headless `claude -p` run
//! is one ([`Claude`]), and a test stands in another, which is what makes the panel testable with no
//! `claude` binary and no network. [`decide`] is the gatekeeper's rule over the panel's verdicts, and
//! it is the same one the merge queue reads: a verdict is `accept`, `reject` or `needs_human`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use letibot_github::mirror::{Mirrors, github_url};
use letibot_github::seat::{Job, Reviewer, Session};
use letibot_github::source::{Comment, Event, Pull, Review, Standing};
use letibot_sessionlog::registry::ReviewFloor;
use letibot_tools::gatekeeper::{Decision, Door, LookedAt, ReviewRequest, Verdict};

use crate::mergequeue::review::claude;

/// What a panel member is handed: the queue's own request, and the diff the seat already read.
#[derive(Debug)]
pub struct Case<'a> {
    /// The queue's request for this pull request - the same shape a merge-queue reviewer is handed,
    /// so a member cannot tell a pull request from a branch.
    pub request: &'a ReviewRequest,
    pub pull: &'a Pull,
    /// The tree this pull request's head is checked out in: one per pull request, from its own
    /// repository's mirror.
    pub tree: &'a Path,
    /// The whole pull request, or the commits after the head last reviewed.
    pub diff: &'a str,
    pub since: Option<&'a str>,
    pub standing: Standing,
    pub comments: &'a [Comment],
}

/// One member of a panel: it judges a pull request and answers the queue's verdict.
pub trait Member: Send {
    /// The name a refused member and the review's own words carry.
    fn name(&self) -> &str;

    /// Judge, and hand the verdict in. A member that cannot judge refuses in words, and that
    /// refusal is the panel's: a panel that lost one member is not the panel that was configured.
    fn judge(&mut self, case: &Case<'_>) -> Result<Verdict, String>;

    /// Answer a mention, when this member holds the conversation about it. `None` is a member
    /// with nothing to say about a question - a judge is not always a correspondent.
    fn answer(&mut self, _session: &Session, _question: &str) -> Option<Result<String, String>> {
        None
    }
}

/// The gatekeeper's rule over a panel: one refusal is a refusal, one undecided member asks a person,
/// and only a panel that every member accepted accepts. An empty panel has decided nothing, which is
/// why it is an `Err` and not an `Accept` - a review nobody made must never read as a pass.
pub fn decide(decisions: &[Decision]) -> Result<Decision, String> {
    if decisions.is_empty() {
        return Err("the panel has no member, so nothing was judged".into());
    }
    if decisions.contains(&Decision::Reject) {
        return Ok(Decision::Reject);
    }
    if decisions.contains(&Decision::NeedsHuman) {
        return Ok(Decision::NeedsHuman);
    }
    Ok(Decision::Accept)
}

/// One GitHub review out of a panel's verdict: the decision first, every member's reasons, and a
/// reason spelled `path:LINE: what is wrong` as an inline comment on that line.
/// The event is always `COMMENT`: an unattended reviewer neither approves nor blocks, so the verdict
/// reaches a person as words rather than as GitHub's own gate.
pub fn review_of_verdict(verdict: &Verdict) -> Review {
    let mut body = format!("verdict: {}", verdict.decision.as_str());
    if verdict.reasons.is_empty() {
        body.push_str(
            " — no member gave a reason, which is a reviewer to look at rather than a verdict to \
             trust",
        );
    }
    // Every member's reason stays in the summary, attributed: an inline comment carries the text
    // after `path:LINE:` and not who said it.
    for reason in &verdict.reasons {
        body.push_str(&format!("\n\n{reason}"));
    }
    if !verdict.looked_at.files.is_empty() {
        body.push_str(&format!(
            "\n\nlooked at: {}",
            verdict.looked_at.files.join(", ")
        ));
    }
    if !verdict.looked_at.commands.is_empty() {
        body.push_str(&format!("\nran: {}", verdict.looked_at.commands.join("; ")));
    }
    Review {
        body,
        event: Event::Comment,
        comments: verdict
            .reasons
            .iter()
            .filter_map(|reason| letibot_github::inline_in(reason))
            .collect(),
    }
}

/// The panel: where a pull request's tree comes from, and the members that judge it.
pub struct Panel {
    mirrors: Mirrors,
    members: Vec<Box<dyn Member>>,
    /// Where a repository is cloned from. GitHub's own URL unless a caller says otherwise, which is
    /// what a test's local repository needs.
    url: Arc<dyn Fn(&str) -> String + Send + Sync>,
    /// The queue's `review_verdict` floor (K5), when the daemon has one: a pull request is judged
    /// by the same policy a branch is, so a floor that parks a branch parks a pull request.
    floor: Option<Arc<dyn ReviewFloor>>,
}

impl Panel {
    pub fn new(mirrors: Mirrors, members: Vec<Box<dyn Member>>) -> Panel {
        Panel {
            mirrors,
            members,
            url: Arc::new(github_url),
            floor: None,
        }
    }

    /// Judge by the daemon's `review_verdict` layers too - `None` is the same panel with none.
    pub fn with_floor(mut self, floor: Option<Arc<dyn ReviewFloor>>) -> Panel {
        self.floor = floor;
        self
    }

    /// Clone from somewhere other than GitHub - the URL of a fixture's repository.
    pub fn with_url(mut self, url: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Panel {
        self.url = url;
        self
    }

    /// The panel's members, by name - what a refusal and a row's words name.
    pub fn members(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.name()).collect()
    }

    /// Every member's verdict, and the gatekeeper's decision over them.
    fn judge(&mut self, case: &Case<'_>) -> Result<Verdict, String> {
        if self.members.is_empty() {
            return Err(
                "the panel has no member, so nobody judged it: the members are the ones this \
                 project configures, and `[queue.claude] binary` in its `leticode.toml` is the one \
                 kind this build has"
                    .into(),
            );
        }
        let mut verdicts: Vec<(String, Verdict)> = Vec::new();
        for member in self.members.iter_mut() {
            match member.judge(case) {
                Ok(verdict) => verdicts.push((member.name().to_string(), verdict)),
                // A member that cannot judge is the panel's failure and not a reason to accept on
                // the others: half a panel is a review nobody configured.
                Err(why) => return Err(format!("{} could not judge: {why}", member.name())),
            }
        }
        let decisions: Vec<Decision> = verdicts.iter().map(|(_, v)| v.decision).collect();
        let decided = decide(&decisions)?;
        // The same floor a branch's verdict goes through (K5): the stricter of the two rungs, so it
        // can park what the panel would have accepted and can never raise a member's reject.
        let decision = letibot_tools::gatekeeper::floored(
            decided,
            self.floor
                .as_ref()
                .and_then(|floor| floor.floor(decided.as_str()))
                .as_deref()
                .and_then(letibot_plugin::review::Floor::parse),
        );
        let mut reasons = Vec::new();
        let mut looked_at = LookedAt::default();
        for (name, verdict) in &verdicts {
            for reason in &verdict.reasons {
                reasons.push(format!("{name}: {reason}"));
            }
            looked_at
                .files
                .extend(verdict.looked_at.files.iter().cloned());
            looked_at
                .commands
                .extend(verdict.looked_at.commands.iter().cloned());
        }
        looked_at.files.sort();
        looked_at.files.dedup();
        looked_at.commands.sort();
        looked_at.commands.dedup();
        if decision != decided {
            reasons.push(format!(
                "the queue's `review_verdict` layer requires at least `{}`, so this review does not \
                 land: the panel's verdict was `{}`",
                decision.as_str(),
                decided.as_str()
            ));
        }
        Ok(Verdict::for_request(
            case.request,
            decision,
            reasons,
            looked_at,
        ))
    }
}

impl Reviewer for Panel {
    fn open(&mut self, pull: &Pull, _standing: Standing) -> Result<Session, String> {
        let url = (self.url)(&pull.repo);
        let tree = self.mirrors.tree(pull, &url)?;
        Ok(Session {
            // A panel is not a session anybody can attach to: the id names the pull request, so a
            // row's words and a refusal both say which one was judged.
            id: format!("panel:{}", pull.key()),
            worktree: tree,
        })
    }

    fn review(&mut self, job: &Job<'_>) -> Result<Review, String> {
        let request = request_of(job);
        let case = Case {
            request: &request,
            pull: job.pull,
            tree: &job.session.worktree,
            diff: job.diff,
            since: job.since,
            standing: job.standing,
            comments: job.comments,
        };
        let verdict = self.judge(&case)?;
        Ok(review_of_verdict(&verdict))
    }

    fn answer(&mut self, session: &Session, question: &str) -> Result<String, String> {
        for member in self.members.iter_mut() {
            if let Some(said) = member.answer(session, question) {
                return said;
            }
        }
        Err(format!(
            "no member of the panel holds a conversation, so `{question}` has nobody to answer it"
        ))
    }
}

/// The queue's request for one pull request: the ask it is reviewed against is what its author said
/// it is, plus what has already been said on it - a reviewer that gets only a diff reviews the wrong
/// change.
pub fn request_of(job: &Job<'_>) -> ReviewRequest {
    let pull = job.pull;
    let mut brief = format!("{}: {}", pull.key(), pull.title.trim());
    if !pull.description.trim().is_empty() {
        brief.push_str(&format!("\n\n{}", pull.description.trim()));
    }
    if !job.comments.is_empty() {
        brief.push_str("\n\nWhat has been said on it so far:\n");
        for comment in job.comments {
            brief.push_str(&format!("- {}: {}\n", comment.author, comment.body.trim()));
        }
    }
    if job.standing == Standing::ReadOnly {
        brief.push_str(
            "\nThis pull request's head is in another repository (a fork): its tree is somebody \
             else's, so run nothing in it and read the diff.",
        );
    }
    ReviewRequest {
        brief,
        branch: pull.branch.clone(),
        base_sha: pull.base_sha.clone(),
        head_sha: pull.head_sha.clone(),
        cwd: job.session.worktree.display().to_string(),
    }
}

/// R116's reviewer as one member: a headless `claude -p` run in the pull request's own tree, whose
/// `structured_output` is the queue's verdict. The prompt is the queue's own, told which door the
/// verdict goes through - a run holds no `review_verdict` tool, so telling it to call one is telling
/// it to fail.
pub struct Claude {
    runs: Arc<claude::Runs>,
    settings: claude::Settings,
    name: String,
    /// What the last run's conversation is resumed by, so a re-review keeps what it read.
    conversation: Option<String>,
}

impl Claude {
    pub fn new(runs: Arc<claude::Runs>, settings: claude::Settings) -> Claude {
        let name = format!(
            "claude ({})",
            settings
                .binary
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| settings.binary.display().to_string())
        );
        Claude {
            runs,
            settings,
            name,
            conversation: None,
        }
    }

    /// How often a run in flight is asked, and how long a member waits past the run's own limit.
    const ASK_EVERY: Duration = Duration::from_millis(100);
}

impl Member for Claude {
    fn name(&self) -> &str {
        &self.name
    }

    fn judge(&mut self, case: &Case<'_>) -> Result<Verdict, String> {
        // No `review_brief` layer here: a pull request's ask is the pull request, and the panel is
        // the daemon's - the queue's stack is asked for a branch's brief, not for this one.
        let prompt = letibot_tools::gatekeeper::review_prompt_with(
            case.request,
            Door::StructuredOutput,
            None,
        );
        let handle = self.runs.start(
            &case.pull.key(),
            &self.settings,
            &prompt,
            case.tree,
            self.conversation.as_deref(),
        )?;
        let deadline_ms = self
            .settings
            .timeout
            .as_millis()
            .try_into()
            .map(|d: u64| crate::mergequeue::gate::now_ms() + d)
            .unwrap_or(u64::MAX);
        loop {
            match self.runs.poll(
                &handle,
                Some(deadline_ms),
                crate::mergequeue::gate::now_ms(),
            ) {
                claude::Polled::Running => std::thread::sleep(Claude::ASK_EVERY),
                claude::Polled::Verdict(answer) => {
                    self.conversation = answer.session_id;
                    self.runs.keep(&[], crate::mergequeue::gate::now_ms());
                    return Ok(Verdict::for_request(
                        case.request,
                        answer.verdict.decision,
                        answer.verdict.reasons,
                        answer.verdict.looked_at,
                    ));
                }
                claude::Polled::Failed(why) | claude::Polled::TimedOut(why) => {
                    // The run is nobody's now, and a member that leaves it in the table holds a
                    // process handle for ever: the queue's own upkeep drops what it no longer waits
                    // for, and this is the same act.
                    self.runs.keep(&[], crate::mergequeue::gate::now_ms());
                    return Err(why);
                }
            }
        }
    }

    fn answer(&mut self, _session: &Session, _question: &str) -> Option<Result<String, String>> {
        // R116's run answers with a verdict and nothing else: `read_stdout` reads the structured
        // output, so a run asked a question in prose is a failed run and not an answer. A mention
        // is answered by a member that holds a conversation, and no kind here does yet.
        None
    }
}

#[cfg(test)]
mod tests;
