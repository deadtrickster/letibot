//! `task` — spawn a subagent to do a subtask and return its result.
//!
//! The tool holds a [`TaskRunner`]; the harness installs the real one (a nested
//! turn with a restricted role) and the default refuses by name. Keeping the seam
//! here means the tool is registered and seatable before the subagent loop lands.
//!
//! # A subagent inherits, and may be downgraded — never upgraded
//!
//! A subagent runs under its parent's permission ruleset, and `access` narrows
//! it further: a survey needs no writes, a formatter needs no network. The
//! [`Downgrade`] is applied to the tools seated, the backend opened and the
//! ruleset, and it is the **union** of the parent's own downgrade and the one
//! asked for, so a downgraded session cannot spawn a wider child by naming a
//! wider role. The role picks which tools; the downgrade says which classes of
//! them are gone whatever the role.
//!
//! # Where it runs
//!
//! `where` names the substrate: `host` (the parent's own boundary, the
//! default) or `firecode` (a VM, firecracker or libvirt). Placement never widens
//! permissions either — a subagent in a VM is still the parent's ruleset, minus
//! the downgrade, inside a boundary. The firecode placement is a declared seam:
//! a build without that backend refuses by name rather than running on the host
//! and calling it a VM.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

// **The child's state and its evidence, from the one place that judges them** — the idle
// plan-check's `todo.rs` (`child_doing`, `ChildDoing`, `child_evidence`), so `task_result` and the
// check's own sentence about the same child cannot come to two different answers. `RunningChild` is
// the reading itself, which this tool carries and does not build.
use crate::builtins::todo::{ChildDoing, RunningChild, child_doing, child_evidence};
use crate::runtime::{Invocation, InvokeCtx, Tool};
pub use crate::schema::Downgrade;
use crate::schema::{Access, ToolSchema};

/// Where a subagent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// The parent's own boundary, whatever it is.
    #[default]
    Host,
    /// A firecode VM. Which hypervisor is the backend's business.
    Firecode,
}

/// The handle's `seam:` line for a placement: what the child's work is made of and where it
/// comes back. Empty for the host, whose work is the worktree itself.
fn placement_seam(placement: Placement) -> String {
    match placement {
        // **No branch on this path.** The VM works on a copy, and firecode delivers the copy's
        // final state as a directory beside it when the VM comes down — the old line promised a
        // branch, which a reader then went looking for (FIRECODE-NOTES).
        Placement::Firecode => {
            "  seam: the VM works on a COPY of the tree; its work comes back as \
             a directory beside that copy when the VM comes down, not as this branch — diff \
             it and take what you want.\n"
                .to_string()
        }
        _ => String::new(),
    }
}

/// **The `where` argument, refused when it cannot be honoured on this host.** A `firecode`
/// placement on a box with no `firecode` used to spawn a child that died at its start —
/// `placing this session in a firecode VM: io: running firecode: No such file or directory` —
/// and the parent learned it only from `task_result`, a round later. Now the call is refused
/// with nothing spawned and `host` named as the way to go on.
///
/// `firecode` is the CLI to look for when the caller names one (a test's stub), `None` for the
/// host's own answer (`$FIRECODE_BIN`, else `firecode` on PATH).
fn placement_arg(
    args: &serde_json::Value,
    firecode: Option<&std::path::Path>,
) -> Result<Placement, Invocation> {
    let placement = match args.get("where").and_then(|v| v.as_str()) {
        None => return Ok(Placement::Host),
        Some(w) => Placement::parse(w).map_err(|e| {
            Invocation::failed(
                "`where` was not understood",
                format!("{e}. Nothing was spawned."),
            )
        })?,
    };
    if placement == Placement::Firecode
        && let Err(why) = match firecode {
            Some(bin) => crate::firecode::firecode_available_at(bin.to_path_buf()),
            None => crate::firecode::firecode_available(),
        }
    {
        return Err(Invocation::failed(
            "this host cannot place a subagent in a VM",
            format!(
                "{why}, so `where: firecode` cannot be honoured. Nothing was spawned. Leave \
                 `where` out (or say `host`) to run the subagent inside your own boundary; \
                 for a VM, the operator puts `firecode` on the daemon's PATH or names it in \
                 $FIRECODE_BIN and restarts the daemon."
            ),
        ));
    }
    Ok(placement)
}

/// The `verify` argument: an acceptance check firecode runs after a VM-placed child finishes
/// ([`TaskSpec::vm_verify`]). Refused off a VM, where nothing would run it — taking it silently
/// would leave the caller believing in a gate that does not exist.
fn verify_arg(
    args: &serde_json::Value,
    placement: Placement,
) -> Result<Option<String>, Invocation> {
    let Some(v) = args
        .get("verify")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    if placement != Placement::Firecode {
        return Err(Invocation::failed(
            "`verify` needs `where: firecode`",
            "the check is run by firecode, after the child finishes and before its VM stops; \
             a child on the host has no VM to run it in. Nothing was spawned. Add `where: \
             firecode`, or leave `verify` out and check the child's work yourself.",
        ));
    }
    Ok(Some(v.to_string()))
}

impl Placement {
    pub fn parse(s: &str) -> Result<Placement, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "host" | "here" => Ok(Placement::Host),
            "firecode" | "vm" | "firecracker" | "libvirt" => Ok(Placement::Firecode),
            other => Err(format!(
                "`{other}` is not a placement. There are two: `host` (the default) and \
                 `firecode` (a VM)"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Placement::Host => "host",
            Placement::Firecode => "firecode",
        }
    }
}

/// What one `task` call asked for, beyond the prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSpec {
    pub role: String,
    pub downgrade: Downgrade,
    pub placement: Placement,
    /// **Which model the child runs on** — the operator's ask, 2026-10-05: *"I want to be
    /// able to have subagents using different models. say you deepseek should be able to
    /// run local model"*, and the other direction too: *"local qwen in main session
    /// should be able to run cloud glm"*.
    ///
    /// `None` inherits the parent's model, which is what every earlier build did and stays
    /// the default. `Some("local")` is the daemon's own server; `Some("provider/model")` a
    /// cloud preset, resolved by the daemon through the same door `/models` uses — so a
    /// name a person can type is a name a child can run on, and a key the picker finds is
    /// a key the spawn finds.
    pub model: Option<String>,
    /// **Where the child's files live**, when `task_start` arranged a tree for it.
    ///
    /// `None` is a plain `task` call, which works in the parent's own workspace — the
    /// behaviour every earlier build had, and the one `task` keeps. `Some` is a
    /// `task_start` call, and the child's workspace is the worktree the runner created
    /// before it spawned the child: the placement is a fact the spawn carries, not a
    /// thing the caller remembers to say. See [`WorktreePlacement`] for why the field
    /// rides the spec rather than a second argument.
    pub worktree: Option<WorktreePlacement>,
    /// **The tree a `firecode` placement COPIES**, when the caller knows a tree the resolution
    /// below would not name.
    ///
    /// `None` — every `task` and `task_start` call — is *the child's own copy source*:
    /// `Config::vm_copy_source` resolves the child's workspace to the repository it is about, and
    /// only falls back to the workspace itself when there is no repository to name. That resolution
    /// is the fix for the shape this field was added for, so a caller with nothing better to say
    /// should say nothing: a session seated a level ABOVE its repositories (the operator's box:
    /// `--workspace /home/dead/Projects/letibot`, whose repository is
    /// `/home/dead/Projects/letibot/letibot`) now copies the repository, where every build before
    /// this field copied the folder — 72.8 GiB of siblings, refused by the ceiling, measured
    /// 2026-10-10.
    ///
    /// The caller that knows BETTER says so here, and the review queue is that caller: a reviewer
    /// reads a branch (`git diff <base>...<branch>`), so it is booted on a copy of the entry's own
    /// repository (`mergequeue::review_copy_source`) and not on the folder the daemon happens to be
    /// seated in — and not necessarily on the repository the session hosting the review is seated
    /// in either, since one queue serves every repository under its workspace. It rides the spec for
    /// the reason [`TaskSpec::worktree`] does: the spawn is the one place that turns a spec into a
    /// session, and a child whose VM copied one tree while the caller's record says another is the
    /// same silent-provenance defect as a model name that does not match.
    pub vm_source: Option<PathBuf>,
    /// **The acceptance check a VM-placed child is held to** — a shell command firecode runs in
    /// the child's project after the child has finished and before the VM stops, whose exit
    /// status is the verdict (`firecode up --verify`).
    ///
    /// Without it a placed child's "done" is its own word, and a child once answered "the recipe
    /// is running in the background" while its VM was about to take that run down. The tiers
    /// that most need placing — an e2e suite, a test stack — are the ones that already have a
    /// runnable check, so the parent says what it is and the harness, not the child, runs it.
    /// The verdict comes back in the child's answer, beside where its work was delivered.
    /// `None` on the host: there is no VM to run it in after the child.
    pub vm_verify: Option<String>,
    /// **The child's name — its slug — when the caller has one to give.**
    ///
    /// The operator: *"lets do slugs for subagents too"*. A background job is named by the
    /// agent's own word for the work (`bash(slug: "release-build")`), and a child was named by its
    /// session id, `s-1789919514688401228-sub-3`, which rings no bell. `task_start` already had a
    /// name for every child it placed — the slug its worktree and branch `agent/<slug>` are cut
    /// from — so that is the child's name, and a plain `task` names its child the same way: the
    /// `slug` argument when the caller gave one, otherwise [`slug_from_prompt`], the derivation
    /// `task_start` makes.
    ///
    /// `None` is *the runner derives it*: the runner is the one that knows the session's other
    /// children, so it is the one that makes the name unique among them (`-2`, `-3`, …, as
    /// [`unique_slug`] does for a worktree). `Some` is `task_start`'s slug, already made unique
    /// against the repository's branches, or a plain `task`'s named one.
    pub slug: Option<String>,
    /// **A tool's own helper, not the model's child** — `digest`'s part readers. The tool that
    /// starts one waits for it and reads its answer itself ([`TaskRunner::consumed`]); the model
    /// never asked for it, never got its handle and is never handed its settlement. So a daemon
    /// restart neither resumes it (the call that wanted its answer is gone) nor re-delivers it,
    /// and it is not listed among the session's children afterwards.
    ///
    /// The operator, 2026-10-14, of a restart that handed the model twelve readers' answers
    /// eighteen hours after their `digest` had read them: *"finished helper/reader children
    /// shouldn't appear in the restored set at all"*.
    pub helper: bool,
}

/// **What one `task_start` call asked for**, beyond the prompt and the [`TaskSpec`].
///
/// The request, before the runner has done anything with it: the slug the worktree and
/// branch are named by, the base the branch is cut from, and whether the exception was
/// asked for (the main checkout rather than a fresh worktree). The runner answers it
/// with a [`WorktreePlacement`] — the path, branch and base SHA that actually exist —
/// and the two are different shapes on purpose: the request is what the caller typed,
/// the placement is what the filesystem now is, and a tool that reported the request as
/// the placement would be the defect this tree keeps finding by looking for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeSpec {
    /// The slug the worktree and branch are named by. Empty is *derive it from the
    /// prompt* — the default, and the one [`slug_from_prompt`] answers.
    pub slug: String,
    /// The base ref the branch is cut from. `None` is the repo's current HEAD, which is
    /// the default and the one a caller that did not look at the branch gets.
    pub base: Option<String>,
    /// **The exception**: work in the main checkout rather than a fresh worktree.
    /// `false` is the default — a `task_start` that did not name the main tree creates
    /// a worktree, and the main tree is the thing it exists to keep the child out of.
    pub main_tree: bool,
    /// **The repository to cut the worktree from**, when it is not the one the session root is
    /// in — a session may run a level above its repositories (`~/Projects`), and then the root
    /// is in no repository at all. Relative to the session root, or absolute. `None` is the
    /// session root's own repository, which is what every earlier build used.
    pub repo: Option<String>,
}

/// **Where a `task_start` child works, as the runner arranged it.**
///
/// The answer to a [`WorktreeSpec`]: the path the child's files live in, the branch
/// they are on, and the base SHA the branch was cut from. It rides the [`TaskSpec`]
/// (as its `worktree` field) rather than a second argument to [`TaskRunner::start`]
/// because the spawn is the one place that turns a spec into a session, and a session
/// whose workspace is not the worktree the tool just reported is the same silent
/// provenance defect as a model name that does not match — the child would be working
/// in the parent's tree while the answer said it was in its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePlacement {
    /// The path the child works in. A worktree path for the default, the main
    /// checkout's path for the exception.
    pub path: String,
    /// The branch the child works on: `agent/<slug>` for a worktree, the main tree's
    /// own branch for the exception.
    pub branch: String,
    /// The base SHA the branch was cut from. The main tree's HEAD for the exception.
    pub base_sha: String,
    /// Whether the child works in the main checkout — the exception, named in the
    /// answer so the operator sees it rather than finds it.
    pub main_tree: bool,
}

/// **The handle a `task_start` call returns, with the placement it arranged.**
///
/// A plain `task` returns a bare handle, because the child works where the parent
/// works and there is nothing else to say. A `task_start` returns this: the handle
/// `task_result` collects by, plus the path, branch and base SHA the child was put in,
/// so the caller and the operator see where the child is working without re-reading
/// the brief. The placement is the point of the tool — a handle that does not say
/// where the child is is a handle the operator has to go and find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHandle {
    /// The handle `task_result` collects by.
    pub handle: String,
    /// Where the child works, as the runner arranged it.
    pub placement: WorktreePlacement,
}

/// **The slug a `task_start` child's worktree and branch are named by**, derived
/// from the prompt when the caller did not name one.
///
/// Short, kebab, and readable: the first few words of the prompt, lowercased, with
/// everything that is not a letter or a digit collapsed to a single hyphen, capped
/// at a length that keeps the path and the branch legible in a `git worktree list`.
///
/// **Deterministic in the prompt, on purpose.** Two different subtasks get two
/// different slugs, and the same subtask asked twice gets the same slug — which is
/// what makes the refusal of an existing path meaningful: a second `task_start` for
/// the same work finds the first one's worktree and says so by name, rather than
/// silently creating a second tree for work that already has one. A caller that
/// wants a different name for the same work names it with the `slug` argument; the
/// derivation is the default, not a lock.
///
/// Pure, so it is testable without a repo: the slug is a fact about the prompt, and
/// the filesystem work that uses it lives in the runner.
pub fn slug_from_prompt(prompt: &str) -> String {
    const MAX: usize = 40;
    const WORDS: usize = 4;
    let mut slug = String::new();
    for word in prompt.split_whitespace().take(WORDS) {
        for c in word.chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.chars().count() > MAX {
        slug = slug.chars().take(MAX).collect::<String>();
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

/// **Why a slug the caller named cannot name a worktree and a branch** (TODO R92.17), or
/// `None`. The slug becomes a directory (`agent-<slug>`) and a branch (`agent/<slug>`), and it was
/// only trimmed: `/` nested directories and could clash with a ref, `..` escaped
/// `.claude/worktrees`, a space made a branch git refuses after the directory was already made.
pub fn slug_refusal(slug: &str) -> Option<String> {
    let why = if slug.contains("..") {
        "it contains `..`, which would leave `.claude/worktrees`"
    } else if slug.contains('/') {
        "it contains `/`, which would nest the worktree's directory and the branch's ref"
    } else if slug.chars().any(char::is_whitespace) {
        "it contains whitespace, which no branch name may"
    } else if slug.starts_with('-') || slug.starts_with('.') || slug.ends_with('.') {
        "it starts with `-` or `.`, or ends with `.`, which a branch name may not"
    } else if slug.ends_with(".lock") {
        "it ends with `.lock`, which a branch name may not"
    } else if slug
        .chars()
        .any(|c| c.is_control() || "~^:?*[\\@{".contains(c))
    {
        "it contains a character a branch name may not (`~ ^ : ? * [ \\ @ {` or a control)"
    } else {
        return None;
    };
    Some(format!(
        "the slug `{slug}` cannot name a worktree and a branch: {why}. Use letters, digits and \
         `-`, e.g. `{}`.",
        slug_from_prompt(slug)
    ))
}

/// **The first free slug from `base`** — `base`, then `base-2`, `base-3`, … — where `taken` says
/// whether one is already used (its worktree's path exists, or its branch does). A slug derived
/// from the prompt collides whenever two briefs begin alike (the `-2` trees on disk were made by
/// hand); a derived one is the default, not a name anybody chose, so the next free one is taken
/// instead of refusing the spawn (TODO R92.17).
pub fn unique_slug(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..1000)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken(s))
        .unwrap_or_else(|| format!("{base}-{}", std::process::id()))
}

/// **One child, by the names it answers to** — its handle (the session id the runner minted) and
/// its slug, the name [`TaskSpec::slug`] gave it.
///
/// `own` says whether THIS session started it. A runner's handle list is shared with the whole
/// tree (`task_result` may collect any handle in it), so a parent's list carries its
/// grandchildren too; a name is looked for among its own children first, which is the
/// *"unique among this session's subagents"* the rule promises.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildName {
    pub handle: String,
    pub slug: Option<String>,
    pub own: bool,
}

impl ChildName {
    /// `fix-login · s-…-sub-3`, or the bare handle for a child nobody named — the job rule
    /// ([`crate::exec::job_label`]), so a child and a job are named the same way in every
    /// sentence about them.
    pub fn label(&self) -> String {
        subagent_label(self.slug.as_deref(), &self.handle)
    }
}

/// **The one spelling of a child's name in a sentence**: `slug · handle`, or the handle alone.
///
/// The same function a job's label is ([`crate::exec::job_label`]), on purpose: the operator's
/// ask was parity with jobs, and two spellings of *how a background thing is called* would be
/// two strings for a reader to learn. The full handle stays in it because it is what the model
/// collects by and what an older reader of the transcript keys on.
pub fn subagent_label(slug: Option<&str>, handle: &str) -> String {
    crate::exec::job_label(slug, handle)
}

/// **A label split back into its name and handle** — the inverse of [`subagent_label`], for a
/// reader of a sentence written either way: `fix-login · s-1-sub-3` is `(Some("fix-login"),
/// "s-1-sub-3")`, and a bare handle (every notice from before children had names) is `(None,
/// handle)`. Only an `s-` handle on the right is a child's, so a label that merely contains a
/// `·` is not misread.
pub fn split_subagent_label(label: &str) -> (Option<&str>, &str) {
    match label.rsplit_once(" · ") {
        Some((slug, handle)) if handle.starts_with("s-") && !slug.trim().is_empty() => {
            (Some(slug), handle)
        }
        _ => (None, label),
    }
}

/// **Which child the caller meant**, over a list of children in hand.
///
/// Pure, so the rules are assertable without a runner — the job rule, kept: **the handle wins**
/// (an exact session id always names its child, whatever another child is called), and **a name
/// that fits more than one child is not a name**. A name is looked for among this session's own
/// children first and only then in the rest of the tree, so a grandchild that happens to share a
/// child's name does not make the child's name ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickedChild {
    /// Exactly one child: its handle.
    One(String),
    /// No child has that handle and none carries that name.
    Unknown,
    /// The name fits more than one child, so it does not identify one.
    Ambiguous(Vec<ChildName>),
}

pub fn pick_child(children: &[ChildName], asked: &str) -> PickedChild {
    let asked = asked.trim();
    if asked.is_empty() {
        return PickedChild::Unknown;
    }
    if children.iter().any(|c| c.handle == asked) {
        return PickedChild::One(asked.to_string());
    }
    let named: Vec<&ChildName> = children
        .iter()
        .filter(|c| c.slug.as_deref() == Some(asked))
        .collect();
    let own: Vec<&ChildName> = named.iter().copied().filter(|c| c.own).collect();
    let pool = if own.is_empty() { named } else { own };
    match pool.as_slice() {
        [] => PickedChild::Unknown,
        [one] => PickedChild::One(one.handle.clone()),
        many => PickedChild::Ambiguous(many.iter().map(|c| (*c).clone()).collect()),
    }
}

/// **A child, from what the caller wrote: its handle, or its slug.**
///
/// `task_result`, `task_message`, `task_resume` and `job_kill` take either, as `job_output` takes a
/// job's id or name. A name that fits exactly one child is its handle; a name two children carry
/// is **refused, naming them** — guessing would act on a child the caller did not name. A string
/// that is neither a handle nor a name of a child here is handed on **unchanged**: the runner's own
/// refusal names it as it was asked, and `task_message` may address a peer session that is no
/// child of this one at all.
pub fn resolve_child(runner: &dyn TaskRunner, asked: &str) -> Result<String, Invocation> {
    resolve_child_in(&runner.children(), asked)
}

/// [`resolve_child`] over a list already in hand.
pub fn resolve_child_in(children: &[ChildName], asked: &str) -> Result<String, Invocation> {
    match pick_child(children, asked) {
        PickedChild::One(handle) => Ok(handle),
        PickedChild::Unknown => Ok(asked.trim().to_string()),
        PickedChild::Ambiguous(many) => Err(ambiguous_child(asked, &many)),
    }
}

/// The refusal for a name more than one child carries. **It names the child the way it was asked
/// for** — the job rule — and lists the candidates by their labels, so the next call can use the
/// handle.
pub fn ambiguous_child(asked: &str, many: &[ChildName]) -> Invocation {
    let lines: String = many.iter().map(|c| format!("  {}\n", c.label())).collect();
    Invocation::failed(
        format!(
            "`{}` is the name of {} subagents, so it does not name one",
            asked.trim(),
            many.len()
        ),
        format!(
            "{lines}\nNothing was done. Use the handle — the name was meant to save looking it \
             up, and a name that fits two subagents cannot do that.\n"
        ),
    )
}

/// **The name a plain `task` child gets**: the `slug` it was given, or the one
/// [`slug_from_prompt`] derives, made unique among the names `taken` already holds (`-2`, `-3`,
/// …) — a child's name is a handle, and a name two children answer to resolves to neither.
pub fn child_slug(given: Option<&str>, prompt: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = match given.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => slug_from_prompt(prompt),
    };
    unique_slug(&base, taken)
}

/// Whether `agent/<slug>`'s worktree path or branch exists in the repository at `under`.
fn slug_taken(under: &str, slug: &str) -> bool {
    if std::fs::symlink_metadata(worktree_path(under, slug)).is_ok() {
        return true;
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(under)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{}", worktree_branch(slug)))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// **The path a `task_start` worktree is created at**, from the repo's workspace and
/// the slug.
///
/// `<workspace>/.claude/worktrees/agent-<slug>` — the same shape the operator's own
/// worktrees take, so a `git worktree list` in the main tree shows the child's tree
/// beside the operator's, and the child is a fact on disk rather than a handle the
/// operator has to go and find. Pure, so the tool and the runner compute the same
/// path from the same inputs: a tool that reported a path the runner did not create
/// would be the same claim-versus-fact defect the rest of this tree refuses.
pub fn worktree_path(workspace: &str, slug: &str) -> String {
    format!("{workspace}/.claude/worktrees/agent-{slug}")
}

/// **The branch a `task_start` worktree is cut onto**, from the slug.
///
/// `agent/<slug>` — the branch is the deliverable, and its name says what it is:
/// an agent's work, cut from the base the caller named (or the repo's HEAD), to be
/// landed by a merge queue rather than pushed. See [`TaskStartTool`] for the rule
/// that it is never pushed.
pub fn worktree_branch(slug: &str) -> String {
    format!("agent/{slug}")
}

/// **The rule a worktree child is given with its task**: the branch is the deliverable, so the
/// work goes onto it, and a finished child's tree is clean.
///
/// Appended to the brief of every `task_start` child that has a fresh worktree (the main-tree
/// exception has no branch of its own to deliver). Asked for by the report of 2026-10-12, from
/// pg-noop's model: *"Seven times tonight a child has ended with its work outside its branch,
/// and each time the recovery was the same two commands; the pattern is now so routine that it
/// belongs in the brief rather than in the salvage."* The daemon checks the same fact when the
/// child's turn ends (`harnessd`'s `harness::deliverable`), so this sentence is the rule and the
/// check is its enforcement — neither stands in for the other.
pub fn worktree_brief(path: &str, branch: &str) -> String {
    format!(
        "[the deliverable] You work in the worktree `{path}` on branch `{branch}`, and the \
         branch is the deliverable: commit your work to it as you go and before you finish. \
         A finished child's worktree is clean (`git status --porcelain` prints nothing), and \
         your final answer names the commit(s)."
    )
}

/// Where a spawned subagent has got to.
///
/// **The reading is the child's PRESENT, and the variant is its turn's record** — two facts, and
/// the whole of the defect this shape fixes is that they are not the same one. `Running`/`Done`
/// are what the daemon's slot holds about the child's FIRST turn (it settles when that turn ends);
/// `child` is what the daemon can see about the child *now*, from the same builder the idle
/// plan-check reads (`harnessd`'s `sessions::child_reading`).
///
/// **Why the present had to be carried at all.** A parent holding a row open on a child could ask
/// `task_result` three times and get one sentence — *"still working"* — about a child that might be
/// working, might have finished its turn and be parked waiting for a word, or might be wedged
/// mid-call. The three are tellable apart ([`ChildDoing`]) and the
/// first of them needs no judgement at all, only a fact: a child with **no turn in flight** is
/// between turns, and the one thing that moves it is `task_message`.
///
/// **`None` is not "nothing is happening"** — it is *this runner keeps no such record*, which is
/// every fake and `NoTaskRunner`, and it renders exactly as it did before this field existed. A
/// reading is only ever attached for a child whose thread is still alive: a thread that has ended
/// has no present to report, and the answer is what the parent wanted anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Still working — the child's turn has not ended. `note` is the daemon's own progress line
    /// from the child's opening phase (see [`TaskRunner::collect`] and `TaskSlot::note`), which is
    /// what it is called: it is NOT the child's last word, and nothing prints it as one.
    Running {
        note: Option<String>,
        /// What the daemon can see about the child right now, or `None` when it keeps no record.
        child: Option<RunningChild>,
    },
    /// Its turn ended, with this answer.
    Done {
        answer: String,
        /// **The child's present, which the answer is not.** `Some` with `working == false` is a
        /// child that is still HERE and between turns — the state whose only door is a message, and
        /// the one a parent could not see before this field.
        child: Option<RunningChild>,
    },
    /// It did not finish, and this is why.
    Failed { why: String },
    /// No subagent by that name was ever spawned here.
    Unknown,
}

impl TaskStatus {
    /// **Which of the states the child is in**, or `None` when this runner keeps no reading — the
    /// one word a listing row and the full sentence both start from.
    pub fn doing(&self) -> Option<ChildDoing> {
        match self {
            TaskStatus::Running { child, .. } | TaskStatus::Done { child, .. } => {
                child.as_ref().map(child_doing)
            }
            TaskStatus::Failed { .. } | TaskStatus::Unknown => None,
        }
    }

    /// **The child's present as one clause** — the state, the evidence, and the move that state
    /// asks for — or `None` when there is no reading to give.
    ///
    /// The state and its evidence come from the tools crate's ONE judgement
    /// (`todo::child_doing` / `todo::child_evidence`), so a parent that has read the idle
    /// plan-check's sentence about this child and then calls `task_result` reads the same state in
    /// the same words. What differs is only the MOVE, because the reader is holding a different
    /// tool: the check names `task_result` (it has no child of its own to steer), and this names
    /// `task_message` — which is the whole of the idle fix.
    pub fn child_now(&self) -> Option<String> {
        let doing = self.doing()?;
        let child = match self {
            TaskStatus::Running { child, .. } | TaskStatus::Done { child, .. } => child.as_ref()?,
            TaskStatus::Failed { .. } | TaskStatus::Unknown => return None,
        };
        let next = match doing {
            // Nothing to do but let it work: this is the sentence that was printed about every
            // child, and it is only true of this one.
            ChildDoing::Working => "it is alive and working, and its answer reaches you on its own",
            // **The idle fix.** A parked child is not stuck and not working; the one door that
            // reaches it is the parent's own message, and before this the parent had to guess that
            // by sending one.
            ChildDoing::BetweenTurns => {
                "nothing is running in it, so `task_message` is what starts a turn for it — a \
                 message reaches a parked child, and it hears it as its parent's words"
            }
            // A turn in flight that has published nothing for the window: the one state a reader
            // is sent to LOOK at, and the one `job_kill` answers.
            ChildDoing::Quiet | ChildDoing::NoEvidence => {
                "check whether it is stuck: `job_kill` stops a child that is wedged, and `transcript` \
                 with `session` set to its handle reads what it did"
            }
        };
        Some(format!(
            "it is {} ({}) — {next}",
            doing.said(),
            child_evidence(child)
        ))
    }
}

/// **What a finished `task_start` child's branch became** — the answer to
/// [`TaskRunner::finished`], in the tool layer's own vocabulary.
///
/// The tool layer does not know what a merge queue is, and that is the point of this enum: a
/// `task_start` child leaves a BRANCH behind, the branch is the deliverable, and where it goes
/// is the runner's business. What the tool needs is the one sentence a person reads — *it is
/// in the queue*, *it was already there*, *there is no branch to land* — and a refusal when
/// the enqueue could not be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    /// The child's branch was put in the queue for landing: this is the entry, or — when the
    /// finish was noticed twice, which is ordinary — the same entry it already had. The two
    /// are one value because they are one FACT: *this branch is in the queue*. A separate
    /// `Already` variant would make a caller branch on a difference nobody can act on, and the
    /// id is carried so a reader can find the row.
    Queued {
        /// The entry's id, which is the child's own handle.
        id: String,
        /// The branch the entry is about.
        branch: String,
    },
    /// There is no branch to land, and this is why — a plain `task` child works in the
    /// parent's own tree, and a `task_start` in the main checkout works on main itself.
    NoBranch { why: String },
}

/// Starts subagents and collects them. **Starting is not running.**
///
/// The trait used to have one method, `run`, which spawned a child and blocked
/// until it answered. The daemon runs a round's calls in order on one thread, so
/// a `task` held every call behind it for as long as the child lived: measured
/// 2026-09-17, a block of `todo_write`, `task` and three `web_search` calls sat
/// unfinished for the fifteen minutes the subagent ran, and the operator — and
/// the model, in its own reasoning — read it as a hang. The operator's reading of
/// the shape: *"task by definition must be background task no? because non
/// background is what? a simple bash?"* Right. A subagent is minutes of work by
/// construction, so the call that starts one returns a handle, the same way
/// `bash --background` does, and the answer is collected by its own call.
pub trait TaskRunner: Send + Sync {
    /// Start the subtask and return the handle it can be collected by. Returns
    /// when the child is **spawned**, not when it is finished.
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String>;

    /// **Arrange the tree, then start the child** — the `task_start` door.
    ///
    /// The difference from [`Self::start`] is the order: the worktree is created
    /// *before* the child is spawned, and the refusal (a path that already exists
    /// or is already a worktree) is answered by this call rather than by a child
    /// that was never started. The operator's ask, in their words: *"we will need
    /// a new tool - task_start or what that will arrange worktree, firecode and
    /// subagent"* — the placement is arranged by the tool, not remembered by the
    /// caller, because the two mistakes this exists to prevent are a child spawned
    /// into the main tree and a child killed instead of corrected for being in the
    /// wrong place.
    ///
    /// Returns the handle **and** the placement the child was put in — the path,
    /// branch and base SHA — so the answer can say where the child is working
    /// without the operator re-reading the brief. A runner that cannot arrange a
    /// tree refuses by name rather than spawning a child in the parent's workspace
    /// and calling it placed: the same claim-versus-fact rule [`Self::kill`] and
    /// [`Self::send`] follow.
    fn start_worktree(
        &self,
        _prompt: &str,
        _spec: &TaskSpec,
        _worktree: &WorktreeSpec,
    ) -> Result<WorktreeHandle, String> {
        Err(
            "this session's runner cannot arrange a worktree: a `task_start` needs the \
             runner that owns the tree to create the worktree before it spawns the child, \
             and this one does not. Nothing was arranged and nothing was spawned."
                .into(),
        )
    }

    /// Where a spawned subagent has got to, waiting up to `timeout` for it to
    /// finish. A zero timeout is a poll.
    fn collect(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus;

    /// **Where a child has got to, for a READER** (`task_result`) — [`Self::collect`] by default.
    /// A runner that records children answers here for one a daemon restart left without a
    /// slot, from its record, so the listing, the single answer and `task_resume` read one
    /// state. Not for a watcher: a recorded settlement is not one arriving now.
    fn look(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus {
        self.collect(handle, timeout)
    }

    /// **This child's settlement has been read** — by `task_result`, which hands the answer to
    /// the model, or by the tool that started a helper and waited for it (`digest`). A runner
    /// that records children marks the record delivered, so a daemon restart does not hand the
    /// same answer over a second time as if it were news (2026-10-14: twelve `digest` readers,
    /// read by their tool the morning before, were announced to the model as just finished).
    /// A runner that records nothing has nothing to mark.
    fn consumed(&self, _handle: &str) {}

    /// The handles this session has started, oldest first — what `task_result`
    /// lists when it is called with no handle, so a model that lost one can find
    /// it without the operator.
    fn started(&self) -> Vec<String> {
        Vec::new()
    }

    /// **The children a handle or a name may resolve to**, with their slugs — what
    /// [`resolve_child`] reads and what the sentences about a child label it by. The default is
    /// [`Self::started`] with no names, so a runner that keeps none resolves handles exactly as
    /// before and labels every child by its handle.
    fn children(&self) -> Vec<ChildName> {
        self.started()
            .into_iter()
            .map(|handle| ChildName {
                handle,
                slug: None,
                own: true,
            })
            .collect()
    }

    /// The slug of one child, `None` for a child nobody named or this runner does not know.
    fn slug_of(&self, handle: &str) -> Option<String> {
        self.children()
            .into_iter()
            .find(|c| c.handle == handle)
            .and_then(|c| c.slug)
    }

    /// `slug · handle` for one child, or the handle — [`subagent_label`] over [`Self::slug_of`].
    fn label_of(&self, handle: &str) -> String {
        subagent_label(self.slug_of(handle).as_deref(), handle)
    }

    /// **A `task_start` child has finished: its branch goes to be landed** — the seam the
    /// merge queue's enqueue hangs on.
    ///
    /// The operator's ask, in their words: *"we need a gated merge to main, and worktree
    /// cleanup"*, and *"a permanent subagent that lazily starts as soon as task_start was
    /// finished and item put into the queue"*. A `task_start` child works on a branch that is
    /// **never pushed** — the tool says so in as many words — so the branch is a deliverable
    /// with nobody to carry it until this call exists. `task_start`'s own docstring used to
    /// carry the `TODO(merge-queue)` that stood here; this is that door.
    ///
    /// **Why the runner and not the tool.** The enqueue needs the brief the child was given,
    /// the placement it worked in and the store the queue lives in, and the tool layer has
    /// none of the three: it has no git, no store and no session. The runner arranged the
    /// tree, so the runner is the one that knows what to hand over — and the tool is left
    /// with the one thing it can honestly say, which is the sentence a person reads.
    ///
    /// **`NoBranch` is the ordinary answer for a plain `task`**, and it is not a failure: a
    /// `task` child works in the parent's own workspace, so there is no branch of its own to
    /// land. A runner that arranges nothing answers that rather than refusing, because a
    /// refusal would put a line about the merge queue under every `task_result` a model ever
    /// reads.
    ///
    /// **`Err` is a real failure and it is said rather than swallowed**, by the rule
    /// [`Self::kill`] and [`Self::send`] follow: a branch reported as queued that is not in
    /// the queue is worse than one that says it could not be, because the parent then
    /// believes the work is on its way to main.
    fn finished(&self, handle: &str) -> Result<Finished, String> {
        let _ = handle;
        Ok(Finished::NoBranch {
            why: "this session's runner arranges no worktree, so a child of it leaves no \
                  branch to land"
                .into(),
        })
    }

    /// **Stop a subagent.** `Ok` is what was done, in the words the operator gets;
    /// `Err` is why it could not be, said rather than swallowed.
    ///
    /// The operator's ruling, when `job_kill` was found to reach only the host's process
    /// table: *"expand"*. A subagent is not a process, so it cannot be reaped the way a
    /// job is — and it is still the same handle-shaped thing a model reaches for a stop
    /// with, which is the rest of the same ruling: *"in a way agent is a background job."*
    ///
    /// **The kill is an interrupt of the child's turn**, because that turn is the only
    /// thing a subagent is doing: it does not own a process, a cgroup or a scope, and the
    /// one door that reaches a turn already running is the session's own steering — the
    /// same door a head's Esc-esc uses. A runner that cannot do that says so rather than
    /// returning a kill it did not perform.
    fn kill(&self, handle: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot stop `{handle}`: a subagent is stopped by \
             interrupting the turn it is running, which only the runner that started it \
             can do. Nothing was stopped."
        ))
    }
    /// **Say something to a session that is still working** — the operator's ruling,
    /// 2026-10-06: *"in the tree all subagents must be addressable by their parents. that is
    /// how live corrections delivered."*
    ///
    /// `Ok` is what was done, in the words the operator gets; `Err` is why it could not be,
    /// said rather than swallowed — the rule [`TaskRunner::kill`] follows, and for the same
    /// reason: a message reported as delivered that nobody heard is worse than one refused,
    /// because the sender then believes the session was corrected.
    ///
    /// **A message is not a second prompt.** It is recorded as an agent's utterance rather
    /// than the operator's, so it can neither authorise the act it races nor be read as
    /// something a person typed. **Both states of a session are reachable, and they are
    /// different deliveries**: one with a turn RUNNING hears it at its next round boundary,
    /// and one BETWEEN turns has a turn started for it with this message as what the turn is
    /// about (the operator's ruling, *"fix task_message - it should enqueue"*). The one thing
    /// a runner must not do is accept a message nothing will read: a handle whose session is
    /// gone, or whose thread has ended, is refused by name.
    ///
    /// **The handle may name a peer, and that is not a widening of this door's job.** The
    /// operator struck the parent-shaped framing of it — *"well gatekeeper is not a child so we
    /// are not doing up"* — so one session sending another session a message is the ordinary
    /// case, and the refusal is for a session that does not exist rather than for one that is
    /// not this session's child.
    fn send(&self, handle: &str, _text: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot message `{handle}`: a subagent is reached through \
             the turn it is running, which only the runner that started it can do. Nothing \
             was sent."
        ))
    }

    /// **Stop every child this session still owns.**
    ///
    /// The operator's design, in their words: *"think about it like it is an erlang
    /// supervision tree. we talk to parents and they own lifecycle."* Two of the tree's edges
    /// are already here — `start` makes a child, and `collect`/`send`/`kill` each talk to one
    /// — and this is the third: **a parent that is being stopped stops its children first**,
    /// so the tree never leaves work computing for nobody. That shape was measured the same
    /// night: a stalled grandchild survived the parent that owned it, because the interrupt
    /// reached exactly one session.
    ///
    /// **The children, not the subtree.** A runner stops what IT started and answers for
    /// those; each child does the same when the stop reaches it, so the walk down is one
    /// level per session and no runner needs to know its grandchildren. That is the whole of
    /// what *"we talk to parents"* means in code, and the reason this is not a `kill` over a
    /// list of handles nobody holds.
    ///
    /// One result per child, in the words [`TaskRunner::kill`] gives, so a caller can say
    /// what was stopped and what refused rather than a count that hides a refusal — the rule
    /// this trait already holds to. A runner that starts nothing returns nothing, which is
    /// the honest answer for a session with no children rather than a failure.
    ///
    /// **Not the restart policy.** Nothing here decides whether a child should be started
    /// again; a parent's decision about what its child's exit MEANS is the second half of the
    /// operator's design and is not built. See the TODO beside `HarnessTaskRunner`'s own
    /// implementation.
    fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
        Vec::new()
    }

    /// **Wait until every child this session started has left its own thread**, up to
    /// `timeout`; `true` when they all have.
    ///
    /// The fourth edge of the same supervision tree [`Self::stop_all`] is the third of, and
    /// the one that was missing: a parent can start a child, talk to it, stop it — and had
    /// **no way to wait for it to be gone**. A child's thread is spawned detached (nothing
    /// keeps a `JoinHandle`), so neither the daemon's shutdown nor a test that drove one
    /// could tell "the child has finished" from "the child is still running", and the
    /// difference is not cosmetic: a thread still inside `libllama` when the process runs its
    /// exit handlers is a SIGSEGV *after every test passed* — measured, `harnessd`'s
    /// `message_between_turns`, where `_dl_fini` finalizes `libggml-cuda` while a child is
    /// mid-`tokenize`. See [`crate::builtins::task::TaskRunner::stop_all`]'s caller and
    /// `Harness::wait_for_children`.
    ///
    /// **A wait, not a stop.** It sends nothing and changes no child's state; it is the
    /// observation the caller makes AFTER it has closed the tree (or stopped the children),
    /// which is why a runner that starts nothing answers `true` rather than refusing: there
    /// is nothing of its own to wait for.
    fn wait_for_children(&self, _timeout: std::time::Duration) -> bool {
        true
    }

    /// **How many children this session started still have a thread** — the ones
    /// [`Self::stop_all`] would stop. A session with any is not idle, whatever its own queue
    /// says: a child it is waiting on can settle at any moment and must find its parent's
    /// reader there. Read by the idle-child policy (`harness::serve_child`), which parks a
    /// subagent only when nothing of its subtree is alive. A runner that starts nothing has
    /// none.
    fn live_children(&self) -> usize {
        0
    }

    /// **Bring back the children a daemon that is gone left running** — called once, when this
    /// session is opened by a daemon.
    ///
    /// The operator, 2026-10-12: *"we should bring subagents back - i cant afford to wait till
    /// they finish. we should record them (session persist anyway) and restart automatically"*.
    /// A runner records each child it starts; this reads those records for children whose task
    /// had not ended when their daemon went down, and resumes each one in its own session — or
    /// says why it did not. A runner that records nothing has nothing to bring back.
    ///
    /// `mode` is the daemon's decision about the down time (the operator: *"if some threshold
    /// passed - say more than 1 hours after the stop, we start to simply remind the model"*):
    /// [`RestoreMode::Resume`] brings each child back, [`RestoreMode::Remind`] only reports it, so
    /// the model can decide and bring it back itself with [`TaskRunner::resume`].
    fn resume_recorded(&self, _mode: RestoreMode) -> Vec<RestartedChild> {
        Vec::new()
    }

    /// **Bring back ONE child a daemon that is gone left unfinished** — the `task_resume` door,
    /// and the same door the automatic restore goes through. `Ok` is the sentence for the model;
    /// `Err` is why it was not brought back, with nothing spawned.
    fn resume(&self, handle: &str) -> Result<String, String> {
        Err(format!(
            "no subagent runner that records children is installed here, so `{handle}` cannot \
             be resumed. Nothing was spawned."
        ))
    }
}

/// **Whether a restart brings children back, or only says what they were doing.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    /// The daemon was down a short while: bring each mid-task child back.
    Resume,
    /// It was down long enough that the world may have moved on: report each child and how to
    /// bring it back with `task_resume`, and leave the decision to the model.
    Remind,
}

/// **What a parent's open did with one child a previous daemon left behind** — see
/// [`TaskRunner::resume_recorded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartedChild {
    /// The child's handle, which is also its session id.
    pub handle: String,
    /// The subtask's first line, for the sentence a person reads.
    pub title: String,
    pub fate: RestartFate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartFate {
    /// It was mid-task, and it is running again in its own session.
    Resumed,
    /// It had already settled and its parent had not been handed the settlement: it is handed
    /// over now, and nothing is respawned.
    Settled,
    /// It was mid-task and was NOT brought back; `why` is the sentence.
    NotResumed { why: String },
    /// It was mid-task, the daemon was down past the restore threshold, and it was left for the
    /// model to bring back (`task_resume`) — where it was working, so the reminder can say.
    Reminded {
        worktree: Option<String>,
        branch: Option<String>,
    },
}

/// The default: no runner, and it says so rather than pretending to have run.
pub struct NoTaskRunner;

impl TaskRunner for NoTaskRunner {
    fn start(&self, _prompt: &str, _spec: &TaskSpec) -> Result<String, String> {
        Err("no subagent runner is installed in this session".into())
    }

    fn collect(&self, _handle: &str, _timeout: std::time::Duration) -> TaskStatus {
        TaskStatus::Unknown
    }
}

pub struct TaskTool {
    runner: Arc<dyn TaskRunner>,
    /// The firecode CLI a `where: firecode` call is checked against, when the caller names
    /// one; `None` is the host's answer ([`crate::firecode::firecode_available`]). A field and
    /// not `$FIRECODE_BIN` for the reason [`crate::firecode::FirecodeSpec::bin`] is one: a test
    /// that sets the variable races every other reader of it in the test binary.
    firecode_bin: Option<std::path::PathBuf>,
}

impl TaskTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskTool {
            runner,
            firecode_bin: None,
        }
    }

    /// Check `where: firecode` against `bin` rather than the host's `$FIRECODE_BIN`/PATH.
    pub fn with_firecode_bin(mut self, bin: impl Into<std::path::PathBuf>) -> Self {
        self.firecode_bin = Some(bin.into());
        self
    }
}

/// The `wake` argument's description, the same for `task` and `task_start` (R93.F22: `task_start`
/// had none, while its result promised a delivery `wake` decides).
const WAKE_ARG: &str = "When the child's answer reaches you. `later` (the default): when your \
     current turn ends, or at once if a todo row needs it. `now`: the moment it finishes, even \
     mid-turn.";

impl Tool for TaskTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task",
            "Start a subtask in a subagent. Give `prompt` (the subtask); optionally \
             `role` to pick the subagent's toolset (defaults to coder), `access` to \
             narrow it BELOW your own permissions (`read-only` for a survey that must \
             not write; or `no-write`, `no-exec`, `no-network`), `where` (`host`, \
             the default, or `firecode` for a VM), and `model` to run the child on a \
             different model than yours (`local`, or `PROVIDER/MODEL` — an unknown \
             name or a missing key is refused at the spawn, naming the fix). A \
             subagent inherits your \
             permissions and can only be given less, never more. This returns as soon \
             as the subagent has STARTED, with a handle — a subagent is minutes of \
             work and this call does not wait for it, so the rest of your calls run \
             while it does. "
                .to_string()
                + super::DELIVERED
                + " `task_result` shows its state or re-reads its answer. **A child that \
             writes code must be told, in its prompt, to END with \
             `sh scripts/check-fmt.sh <the ref it was cut from>`**: code a child has \
             written, built and TESTED can still be unformatted, because `cargo fmt` \
             is in no brief and CI's `Format (the files this change touches)` job is \
             the only thing that reads it. The same one line goes for `cargo test \
             --workspace --no-run` when the child changes a type's SHAPE — one change \
             reaches callers in crates the child never ran.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The subtask for the subagent."},
                    "role": {"type": "string", "description": "The subagent's role. Defaults to coder."},
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary) or `firecode` (a VM: a copy of this workspace, permission checks off inside, so a child there asks nobody). Placement never changes permissions."},
                    "verify": {"type": "string", "description": "With `where: firecode`: a shell command firecode runs in the child's project AFTER the child finishes and before its VM stops - the e2e or test command that says whether the work is done. Its exit status is the verdict, reported in the child's answer with its log; the child is told nothing it can fake. Refused without `where: firecode`."},
                    "model": {"type": "string", "description": "Run the child on a different model than yours (`local`, or `PROVIDER/MODEL` — an unknown name or a missing key is refused at the spawn, naming the fix)."},
                    "slug": {"type": "string", "description": "A short name for the child (letters, digits, `-`), your word for the work — `task_result`, `task_message` and `job_kill` take it in place of the handle. Defaults to a derivation from the prompt; made unique among your subagents."},
                    "wake": {"type": "string", "enum": ["now", "later"], "description": WAKE_ARG}
                },
                "required": ["prompt"]
            }),
            // `Session`, not `Exec`: `task` runs no host command itself — it delegates
            // to a child turn whose own gate governs its write/exec/network calls. As
            // `Exec` it would hit the operator's rule that exec always asks (a subagent
            // spawn is not a shell, and opencode does not gate it), and a subagent that
            // could not even be spawned without a head would never run.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task needs a prompt",
                "call `task` with `prompt` set to the subtask.",
            );
        };
        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("coder");
        let downgrade = match args.get("access").and_then(|v| v.as_str()) {
            None => Downgrade::none(),
            Some(a) => match Downgrade::parse(a) {
                Ok(d) => d,
                Err(e) => {
                    return Invocation::failed(
                        "`access` was not understood",
                        format!("{e}. Nothing was spawned."),
                    );
                }
            },
        };
        let placement = match placement_arg(args, self.firecode_bin.as_deref()) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let vm_verify = match verify_arg(args, placement) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        // R79: when the child's answer reaches the model — refused by name before anything
        // spawns, as `bash` does.
        let wake = match super::bash::wake_arg(args) {
            Ok(w) => w,
            Err(refused) => return refused,
        };
        // **The child's name**, when the caller gave one: held to `task_start`'s rule for a slug,
        // so one name is spelled one way whichever tool started the child. Absent, the runner
        // derives it from the prompt as `task_start` does.
        let slug = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(s) = &slug
            && let Some(why) = slug_refusal(s)
        {
            return Invocation::failed(
                format!("the slug `{s}` is not a name"),
                format!("{why} Nothing was spawned."),
            );
        }
        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
            model: args
                .get("model")
                .and_then(|v| v.as_str())
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
            // A plain `task` works in the parent's workspace: no worktree is arranged.
            worktree: None,
            // And a VM placement copies that workspace's own copy source, which is the repository
            // it is about whenever there is one to name (`Config::vm_copy_source`). The one caller
            // that knows a tree the resolution would not name sets it itself
            // (`Harness::serve_reviews`); a model's `where: firecode` has no way to name a tree it
            // has not looked at.
            vm_source: None,
            vm_verify,
            slug,
            // A child the model started is the model's, never a tool's helper.
            helper: false,
        };
        let handle = match self.runner.start(prompt, &spec) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, "the subagent did not start."),
        };
        // **The name the runner gave it**, for every sentence below: `slug · handle`.
        let label = self.runner.label_of(&handle);
        ctx.progress(&format!("subagent {label} started"));
        if let Some(w) = wake {
            ctx.want_completion(&handle, w);
        }
        // **`Backgrounded`, not `Ok`** — the same reason `bash --background`
        // gives: the outcome names a fact about the world, *this is running and
        // has not answered yet*, and an `Ok` here would be grounding for an
        // answer nothing has produced.
        Invocation::backgrounded(
            handle.clone(),
            std::time::Duration::ZERO,
            letibot_transcript::Backgrounding::Asked,
            format!(
                "`task_result` with task=\"{handle}\" shows where it has got to.{}",
                crate::builtins::todo::attach_now(ctx, "child", &handle)
            ),
            format!(
                "started subagent `{label}` as `{}`.\n  subtask: {}\n\nIt is working \
                 now, and this call did not wait for it. {} `task_result` with \
                 task=\"{handle}\" shows where it has got to.",
                match &spec.model {
                    // **The model is named when the child was given one** — a spawn that
                    // prints only the role would leave the reader to guess which model is
                    // answering, and the whole point of the argument is that they chose.
                    Some(m) => format!("{m} (as a {})", spec.role),
                    None => spec.role.clone(),
                },
                first_line(prompt),
                super::DELIVERED,
            ),
        )
    }
}

/// The subtask's first line, for a one-line echo. The whole prompt back would be
/// the model's own bytes returned to it as a result.
fn first_line(prompt: &str) -> String {
    let l = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if l.chars().count() <= 100 {
        return l.to_string();
    }
    format!("{}…", l.chars().take(100).collect::<String>())
}

/// `task_start` — arrange a subagent's placement *before* it spawns it.
///
/// The operator's ask, in their words: *"we will need a new tool - task_start or
/// what that will arrange worktree, firecode and subagent"*. The two mistakes of a
/// previous session are its spec: a child was spawned into the main tree, and then
/// that child was killed instead of corrected. Placement must live in the tool, not
/// in the caller's memory — so this tool creates the worktree first, refuses a path
/// that already exists by name and without spawning anything, and only then starts
/// the child in the tree it just made.
///
/// **The default is never the main tree.** A `task_start` that did not name the main
/// checkout creates a fresh worktree at `<workspace>/.claude/worktrees/agent-<slug>`
/// on branch `agent/<slug>`, cut from the base the caller named (or the repo's HEAD).
/// Working in the main checkout is the exception, and it requires the explicit
/// `main_tree` argument — the same shape as `where: firecode`, a declared seam rather
/// than a default.
///
/// **The build cache is shared deliberately.** The child's environment carries
/// `CARGO_TARGET_DIR` pointing at the main tree's `target/`, because cargo's own lock
/// serialises concurrent builds and a fresh worktree otherwise pays a cold build of
/// the whole graph. The runner is where that environment is set — the tool layer has
/// no git and no exec of its own.
///
/// **The branch is the deliverable, and it is never pushed.** The tool does not push,
/// and will not: the branch is to be landed by a merge queue.
///
/// **TODO(merge-queue): CLOSED — the branch is enqueued when the child finishes.**
///
/// This docstring used to carry the seam: *"the merge queue does not exist yet. The branch is
/// the deliverable and the landing is somebody else's job; this tool leaves the branch where it
/// is and says so."* The queue exists, and the door is [`TaskRunner::finished`]: when a
/// `task_start` child finishes, the runner mints the entry — the branch, the base SHA, the
/// priority, the `needs` and **the brief the child was given**, which is the one thing the
/// reviewer reads — and writes it to the session store the merge-queue daemon serves.
///
/// **The tool does not do it, and the split is deliberate.** The enqueue needs the brief, the
/// placement and the store, and this layer has no git, no store and no session; it has the
/// handle and the sentence. So the tool asks the runner when it sees the child finish
/// (`task_result` on a `Done`) and reports what the runner answered — and the runner ALSO
/// enqueues at the child's own settlement, because a branch whose answer is never collected is
/// still a branch that finished. One entry, one id (the handle), so the two notices are one
/// act rather than two.
///
/// **The branch is still never pushed by this tool**, and that has not changed: the queue
/// fast-forwards main and pushes, and the tool's rule is that the deliverable is a branch.
///
/// **A child in the wrong PLACE is corrected with `task_message`, not killed.**
/// Killing is for wrong work. A child that is in the wrong tree is a placement
/// mistake, and the remedy is to tell it where to be — the same live correction
/// `task_message` exists for — not to throw the work away.
pub struct TaskStartTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskStartTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskStartTool { runner }
    }
}

impl Tool for TaskStartTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_start",
            "Arrange a subagent's placement BEFORE it spawns it: create a git worktree \
             for the child, then start it in that worktree. Give `prompt` (the subtask); \
             optionally `role`, `access`, `where` and `model` exactly as `task` takes \
             them, plus `base` (the ref the branch is cut from, default the repo's \
             current HEAD), `slug` (the worktree and branch name, default derived from \
             the prompt), and `main_tree` (work in the main checkout instead of a fresh \
             worktree — the exception, off by default). By default the child works in \
             a fresh worktree at `<workspace>/.claude/worktrees/agent-<slug>` on branch \
             `agent/<slug>`, sharing the main tree's cargo build cache; a path that \
             already exists is refused by name and nothing is spawned. The branch is \
             the deliverable and is NEVER pushed — it is to be landed by a merge queue. \
             A child in the wrong PLACE is corrected with `task_message`, not killed: \
             killing is for wrong work. "
                .to_string()
                + super::DELIVERED
                + " `task_result` shows its state or re-reads its answer.",
            json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The subtask for the subagent."},
                    "role": {"type": "string", "description": "The subagent's role. Defaults to coder."},
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary) or `firecode` (a VM: a copy of this workspace, permission checks off inside, so a child there asks nobody). Placement never changes permissions."},
                    "verify": {"type": "string", "description": "With `where: firecode`: a shell command firecode runs in the child's project AFTER the child finishes and before its VM stops - the e2e or test command that says whether the work is done. Its exit status is the verdict, reported in the child's answer with its log; the child is told nothing it can fake. Refused without `where: firecode`."},
                    "model": {"type": "string", "description": "Run the child on a different model than yours (`local`, or `PROVIDER/MODEL` — an unknown name or a missing key is refused at the spawn, naming the fix)."},
                    "base": {"type": "string", "description": "The ref the branch is cut from. Defaults to the repo's current HEAD."},
                    "slug": {"type": "string", "description": "The slug the worktree and branch are named by. Defaults to a derivation from the prompt."},
                    "main_tree": {"type": "boolean", "description": "Work in the main checkout instead of a fresh worktree. The exception; off by default."},
                    "repo": {"type": "string", "description": "The repository to work in, when it is not the one the session root is in (a session above its repositories names one). Relative to the session root, or absolute."},
                    "wake": {"type": "string", "enum": ["now", "later"], "description": WAKE_ARG}
                },
                "required": ["prompt"]
            }),
            // `Session`, exactly as `task` is: this arranges a tree and delegates to a
            // child turn whose own gate governs its write/exec/network calls. The git
            // work happens in the runner, not here — the tool layer has no git and no
            // exec of its own.
            Access::Session,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(prompt) = args.get("prompt").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_start needs a prompt",
                "call `task_start` with `prompt` set to the subtask.",
            );
        };
        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("coder");
        let downgrade = match args.get("access").and_then(|v| v.as_str()) {
            None => Downgrade::none(),
            Some(a) => match Downgrade::parse(a) {
                Ok(d) => d,
                Err(e) => {
                    return Invocation::failed(
                        "`access` was not understood",
                        format!("{e}. Nothing was arranged and nothing was spawned."),
                    );
                }
            },
        };
        // Checked before the worktree is arranged: a refusal here leaves nothing behind.
        let placement = match placement_arg(args, None) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let vm_verify = match verify_arg(args, placement) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        // R93.F22: `task_start` takes `wake` as `task` does, refused by name before anything is
        // arranged.
        let wake = match super::bash::wake_arg(args) {
            Ok(w) => w,
            Err(refused) => return refused,
        };
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let base = args
            .get("base")
            .and_then(|v| v.as_str())
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty());
        let slug_arg = args
            .get("slug")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let main_tree = args
            .get("main_tree")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let repo = args
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|r| r.trim().trim_end_matches('/').to_string())
            .filter(|r| !r.is_empty() && r != ".");

        // **The slug, derived from the prompt when the caller did not name one.** The
        // derivation is the default, not a lock: a caller that wants a different name
        // for the same work names it with `slug`. A named one is checked before anything is
        // made (R92.17).
        if let Some(s) = &slug_arg
            && let Some(why) = slug_refusal(s)
        {
            return Invocation::failed(
                format!("the slug `{s}` is not a name"),
                format!("{why} Nothing was arranged and nothing was spawned."),
            );
        }
        let named = slug_arg.is_some();
        let mut slug = slug_arg.unwrap_or_else(|| slug_from_prompt(prompt));

        // **The workspace, or the refusal that names its absence.** A `task_start`
        // with no workspace to arrange a worktree in is a call that cannot be done,
        // and it says so rather than guessing at a path.
        let Some(workspace) = ctx.backend.workspace_path() else {
            return Invocation::failed(
                "task_start needs a workspace",
                "this session has no workspace to arrange a worktree in. Nothing was \
                 arranged and nothing was spawned.",
            );
        };

        // **The refusal of an existing path, by name and without spawning anything.**
        // The default is a fresh worktree, and a path that already exists — a
        // directory, or a worktree a previous `task_start` made — is refused here
        // rather than by a child that was never started. The main tree is the
        // exception and does not create a path, so it is not checked.
        if !main_tree {
            // Under the named repository, when there is one — the runner arranges it there.
            let under = match &repo {
                Some(r) if r.starts_with('/') => r.clone(),
                Some(r) => format!("{workspace}/{r}"),
                None => workspace.clone(),
            };
            // A derived slug that is taken becomes the next free one (`-2`, `-3`, …).
            if !named {
                slug = unique_slug(&slug, |s| slug_taken(&under, s));
            }
            let path = worktree_path(&under, &slug);
            if std::fs::symlink_metadata(&path).is_ok() {
                return Invocation::failed(
                    format!("the path {path} already exists"),
                    format!(
                        "`{path}` is already a directory or a worktree, so a fresh \
                         worktree cannot be created there. Nothing was arranged and \
                         nothing was spawned. Use a different `slug`, or `main_tree: \
                         true` to work in the main checkout (the exception)."
                    ),
                );
            }
        }

        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
            model,
            // The runner fills this after the git work: the placement is a fact the
            // spawn carries, and the tool does not invent one the runner did not make.
            worktree: None,
            // A `task_start` works in the worktree it just arranged, so a VM placement copies
            // that — the child's own workspace, which is what `None` means.
            vm_source: None,
            vm_verify,
            // **The child is named after its worktree** — the slug its branch `agent/<slug>` is
            // cut from, already made unique against the repository above. A main-tree child has
            // no branch of its own and is still named by the same slug.
            slug: Some(slug.clone()),
            // A child the model started is the model's, never a tool's helper.
            helper: false,
        };
        let worktree_spec = WorktreeSpec {
            slug,
            base,
            main_tree,
            repo,
        };

        let handle = match self.runner.start_worktree(prompt, &spec, &worktree_spec) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, "the subagent did not start."),
        };
        let label = self.runner.label_of(&handle.handle);
        ctx.progress(&format!(
            "subagent {label} started in {}",
            handle.placement.path
        ));
        if let Some(w) = wake {
            ctx.want_completion(&handle.handle, w);
        }

        // **The placement is in the answer.** The handle names the path, the branch
        // and the base SHA, so the caller and the operator see where the child is
        // working without re-reading the brief. The main tree is named as the
        // exception when it was the exception, and a firecode spawn says the VM got
        // a copy rather than pretending it is the same directory.
        let where_line = if handle.placement.main_tree {
            "the main checkout (the exception — a fresh worktree is the default)".to_string()
        } else {
            "a fresh worktree".to_string()
        };
        let seam_line = match handle.placement.main_tree {
            true => String::new(),
            false => placement_seam(spec.placement),
        };
        // **`Backgrounded`, not `Ok`** — the same reason `task` gives: the outcome
        // names a fact about the world, *this is running and has not answered yet*.
        Invocation::backgrounded(
            handle.handle.clone(),
            std::time::Duration::ZERO,
            letibot_transcript::Backgrounding::Asked,
            format!(
                "`task_result` with task=\"{0}\" shows where it has got to.{1}",
                handle.handle,
                crate::builtins::todo::attach_now(ctx, "child", &handle.handle)
            ),
            format!(
                "started subagent `{}` as `{}` in {}.\n  subtask: {}\n  path: {}\n  \
                 branch: {}\n  base: {}\n{seam_line}\nThe branch is the deliverable and \
                 is NOT pushed; it is to be landed by a merge queue. It is working now, \
                 and this call did not wait for it. {} `task_result` with task=\"{}\" \
                 reports where it has got to. A child in the wrong PLACE is corrected \
                 with `task_message`, not killed: killing is for wrong work.",
                label,
                match &spec.model {
                    Some(m) => format!("{m} (as a {})", spec.role),
                    None => spec.role.clone(),
                },
                where_line,
                first_line(prompt),
                handle.placement.path,
                handle.placement.branch,
                handle.placement.base_sha,
                super::DELIVERED,
                handle.handle,
            ),
        )
    }
}

/// `task_result` — collect a subagent `task` started.
pub struct TaskResultTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskResultTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskResultTool { runner }
    }
}

impl Tool for TaskResultTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_result",
            "Look at a subagent started by `task`. Its answer is delivered to you \
             (when your current turn ends, or at once if a todo row needs it or wake is \
             \"now\"); this shows its state or re-reads the answer. Give `task` (the handle `task` returned) to see where it has got to. \
             `timeout_ms` blocks only for a child this session is not already being \
             told about. \
             The three endings are three different outcomes: it answered, it failed, or \
             its turn has not ended — and in that third case the answer says WHICH state \
             the child is in, because *still working* was printed about a child that was \
             working, one that was between turns and one that had wedged: `working` (leave \
             it), `between turns` (nothing is running in it — `task_message` starts a turn \
             for it), `quiet` (a turn is in flight and nothing has come out of it — check \
             whether it is stuck). Call with no `task` to list the subagents this \
             session started.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The handle `task` returned, or the child's slug. Omit to list this session's subagents."},
                    "timeout_ms": {"type": "integer", "description": "Block up to this long for a subagent whose answer is not already being delivered to you. Omit to report its state without waiting."}
                }
            }),
            // Reading the answer of a child that has already been governed by its
            // own gate. Nothing here reaches the host.
            Access::Read,
        )
    }

    fn invoke(&self, ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(handle) = args.get("task").and_then(|v| v.as_str()) else {
            let started = self.runner.children();
            if started.is_empty() {
                return Invocation::ok(
                    "this session has started no subagents. `task` starts one and \
                     returns the handle to collect it by."
                        .to_string(),
                );
            }
            let mut out = format!("{} subagent(s) started here:\n", started.len());
            for c in &started {
                let h = &c.handle;
                let status = self.runner.look(h, std::time::Duration::ZERO);
                // **The listing says the STATE, not liveness** — a row that says `running` about a
                // child between turns is the sentence this change exists to stop printing, and the
                // word comes from the same judgement the single-handle answer uses
                // (`ChildDoing::said`). A runner that keeps no reading keeps the old words.
                let state = match &status {
                    TaskStatus::Running { note, .. } => match (status.doing(), note) {
                        (Some(doing), Some(n)) => format!("{} — {n}", doing.said()),
                        (Some(doing), None) => doing.said().to_string(),
                        (None, Some(n)) => format!("running — {n}"),
                        (None, None) => "running".into(),
                    },
                    TaskStatus::Done { .. } => "answered".into(),
                    TaskStatus::Failed { .. } => "failed".into(),
                    TaskStatus::Unknown => "unknown".into(),
                };
                out.push_str(&format!("  {} — {state}\n", c.label()));
            }
            return Invocation::ok(out);
        };
        // **The handle, or the child's name** — the job rule: the id wins, a name two children
        // carry is refused naming them, and anything else goes on as it was written.
        let handle = match resolve_child(self.runner.as_ref(), handle) {
            Ok(h) => h,
            Err(refused) => return refused,
        };
        let handle = handle.as_str();
        let label = self.runner.label_of(handle);
        // A deadline always applies, and its expiry is its own outcome rather
        // than being reported as completion — `job_wait`'s rule, for the same
        // reason: a wait that ran out and a child that answered are two facts.
        let timeout = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(std::time::Duration::from_millis)
            .unwrap_or(std::time::Duration::ZERO);
        // **R23, which `job_wait` got and this tool did not.** The daemon watches every child
        // and hands its answer to the model as a turn of its own, so a wait on a watched child
        // holds the floor for a deadline while the answer is already on its way. The operator,
        // after a stroppy session spent 240 s inside `task_result`: *"why it decided to wait
        // for it explicitly if we have completion events"*, and *"on other hosts they just
        // start subagents and dont wait"*. So a watched child is looked at, not waited on: an
        // answer or a failure already in is returned as ever, and a child still working is
        // reported as such at once. Unwatched handles — another session's, a runtime with no
        // watcher — keep the blocking wait they asked for.
        let timeout = if !timeout.is_zero() && ctx.completion_delivered(handle) {
            // **And the wait is the model saying it wants this one** (R79): the answer is
            // delivered the moment it lands rather than held for the next break, which is the
            // promise the sentence below makes.
            ctx.want_completion(handle, crate::runtime::WantedNow::Awaited);
            let status = self.runner.look(handle, std::time::Duration::ZERO);
            match &status {
                TaskStatus::Running { note, .. } => {
                    // **The steering clause, in a value rather than inside the `format!`**: the
                    // reading's own sentence when the daemon has one, and the pair of tools that
                    // have always been named when it has none (every fake, and `NoTaskRunner`).
                    let next = status.child_now().unwrap_or_else(|| {
                        "`task_message` steers it and `job_kill` stops it".into()
                    });
                    return Invocation::ok(format!(
                        "nothing to wait for: {}, and its answer reaches you on its own when it \
                         finishes — the daemon is watching it and, because you asked, hands you \
                         the result the moment it lands, unprompted. Waiting here would hold the floor \
                         while the answer is already on its way. Carry on with something \
                         else; {next}.{}",
                        running_said(&label, &status),
                        // **The spawn's own line, and it is named as the spawn's.** It used to
                        // print as *`(last: …)`* — the child's last word — and it is not one: the
                        // note is written by the progress callback during the child's OPENING
                        // phase and never again. The child's actual last word is in the reading
                        // above, so the note is labelled with what it really is.
                        note.as_ref()
                            .map(|n| format!(" (the spawn reported: {n})"))
                            .unwrap_or_default()
                    ));
                }
                _ => std::time::Duration::ZERO,
            }
        } else {
            timeout
        };
        let status = self.runner.look(handle, timeout);
        // **An answer or a failure read here has been handed to the model** — the delivery the
        // watcher would otherwise make — so the runner marks it read, and a daemon restart does
        // not announce it again as news.
        if matches!(status, TaskStatus::Done { .. } | TaskStatus::Failed { .. }) {
            self.runner.consumed(handle);
        }
        match status {
            TaskStatus::Done { ref answer, .. } => {
                // **The child has finished, so its branch goes to be landed.** This is the one
                // door the tool layer has onto a child's finish, and `task_start`'s own answer
                // already sends the model here (*"call `task_result` with task=…"*), so the
                // ordinary path reaches it. The runner does the work and says what happened;
                // a plain `task` child has no branch of its own and answers `NoBranch`, which
                // is why nothing is appended for it and every existing answer reads exactly as
                // it did.
                //
                // **The answer comes first and the queue line after it**, because the answer
                // is what the model asked for and the queue line is news about it.
                //
                // **And the child's PRESENT comes after both, when the daemon has one.** An
                // answer is the child's first turn's record: a child that answered and is still
                // here is a child that is parked, and the only door onto a parked child is its
                // parent's message. That is the sentence the operator had to learn by sending one
                // — *"it was between turns … a turn has been started for it"* — and it belongs
                // where the parent is already reading.
                let now = status.child_now().map(|said| format!("\n\n[child] {said}"));
                match self.runner.finished(handle) {
                    Ok(Finished::Queued { id, branch }) => Invocation::ok(format!(
                        "{answer}\n\n[merge queue] `{branch}` is enqueued as `{id}` and is \
                         waiting to be landed: the gatekeeper reviews it against the brief it \
                         was given, then the queue rebases it at the tip of main and runs the \
                         gate. The worktree stays until it lands.{}",
                        now.unwrap_or_default()
                    )),
                    Ok(Finished::NoBranch { .. }) => {
                        Invocation::ok(format!("{answer}{}", now.unwrap_or_default()))
                    }
                    // **A failure is said, not swallowed.** The branch is not pushed and nobody
                    // else carries it, so a parent told nothing would believe the work was on
                    // its way to main when it is sitting on a branch in a worktree.
                    Err(why) => Invocation::ok(format!(
                        "{answer}\n\n[merge queue] the branch of `{label}` was NOT enqueued: \
                         {why} It is still on its branch in its worktree, and nothing has been \
                         lost — but nothing will land it until it is enqueued.{}",
                        now.unwrap_or_default()
                    )),
                }
            }
            TaskStatus::Failed { why } => Invocation::failed(
                format!("subagent `{label}` did not finish"),
                format!(
                    "{why}\n\nNothing of its work is lost: it has its own session, \
                         and `transcript` with session=\"{handle}\" reads it."
                ),
            ),
            // **The sentence that said one thing about three states.** *"Still working"* was
            // printed about a child that was working, about one whose turn had ended and was
            // parked waiting for a word, and about one that had wedged mid-call — the operator's
            // own measurement, three asks and one sentence. The headline now names the state, and
            // the body carries what the daemon can see and the move that state asks for. A runner
            // that keeps no reading gets the old words, byte for byte.
            // R93.F18: a look (no `timeout_ms`) at a child this session is told about is not a
            // wait that ran out, and "call again with a longer timeout_ms" contradicted every
            // text that says its answer is delivered.
            TaskStatus::Running { .. } if timeout.is_zero() && ctx.completion_delivered(handle) => {
                Invocation::abstained(
                    format!("subagent `{label}` is {}", doing_word(&status)),
                    format!(
                        "`{label}` has not answered yet; its answer is delivered to you when it \
                         finishes.{}",
                        status
                            .child_now()
                            .map(|said| format!(" Right now {said}."))
                            .unwrap_or_default()
                    ),
                )
            }
            TaskStatus::Running { ref note, .. } => Invocation::abstained(
                format!("subagent `{label}` is {}", doing_word(&status)),
                format!(
                    "the deadline passed and `{label}` has not answered yet{}. {} This is \
                     the wait ending, not the subagent ending — call `task_result` again \
                     with a longer `timeout_ms`, or do something else and collect it \
                     later.",
                    note.as_ref()
                        .map(|n| format!(" (the spawn reported: {n})"))
                        .unwrap_or_default(),
                    status
                        .child_now()
                        .map(|said| format!("Right now {said}."))
                        .unwrap_or_default()
                ),
            ),
            TaskStatus::Unknown => Invocation::failed(
                format!("no subagent called `{handle}` was started here"),
                "call `task_result` with no `task` to list the ones that were.".to_string(),
            ),
        }
    }
}

/// **The state of a child whose turn has not ended, in words** — `working`, `between turns`,
/// `quiet` or `showing nothing yet` when the daemon has a reading, and `still working` when it has
/// none (every fake, and `NoTaskRunner`).
///
/// One function, because the R23 sentence and the deadline sentence are two doors onto one child
/// and must not disagree about which state it is in — the same rule [`TaskStatus::child_now`]
/// follows for the body.
fn doing_word(status: &TaskStatus) -> String {
    match status.doing() {
        Some(doing) => doing.said().to_string(),
        None => "still working".to_string(),
    }
}

/// **`task_result`'s half of the child's state, with the name attached** — the state word and the
/// child's handle, for a sentence that has to say which child it is about.
fn running_said(label: &str, status: &TaskStatus) -> String {
    format!("`{label}` is {}", doing_word(status))
}

/// `task_message` — say something to a session, running or not.
///
/// The third thing a parent does to work it handed off: `task` starts a child,
/// `task_result` reads it, `job_kill` stops it, and this **steers** it. Sibling by shape and
/// by reason — see [`TaskRunner::send`] for why a message is not a second prompt in the
/// recipient's session, for the two deliveries (a running turn and a session between turns),
/// and for why a session that is gone is refused rather than queued.
///
/// **And it is not only for children.** The operator's correction to this channel, verbatim:
/// *"well gatekeeper is not a child so we are not doing up"* — a session sends another session
/// a message, laterally, and the handle may name **any live session**: a subagent this one
/// started, or a peer (the session a review is hosted by, the session that queued the work).
/// That is the same door and the same act; only the wording of the refusal was child-shaped.
pub struct TaskMessageTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskMessageTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskMessageTool { runner }
    }
}

impl Tool for TaskMessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_message",
            "Say something to a session — a live correction. Give \
             `task` (the handle `task` returned, or the id of any live session) and `text` \
             (what to say). It is not a second prompt: the session hears it as your message \
             and carries on. A session whose turn is running hears it at its next round \
             boundary; a session that has answered and is waiting is woken and runs a turn \
             for it. Refused by name if no session of that id is live. Read a subagent with \
             `task_result`; stop one with `job_kill`.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The handle `task` returned (or the child's slug), or the id of a live session to address."},
                    "text": {"type": "string", "description": "What to say to the session. It keeps working; this steers what it does next."}
                },
                "required": ["task", "text"]
            }),
            // `Session`, exactly as `task` and `task_result` are: this reaches a child's own
            // session and nothing on the host. The child's own gate governs whatever it does
            // with what it is told — a parent cannot use a message to launder a capability.
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(handle) = args.get("task").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_message needs the `task` handle",
                "call `task_message` with `task` set to the handle `task` returned.",
            );
        };
        let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_message needs `text`",
                "call `task_message` with `text` set to what the subagent should be told.",
            );
        };
        let text = text.trim();
        if text.is_empty() {
            return Invocation::failed(
                "the message was empty",
                "an empty message is not a correction. Nothing was sent.",
            );
        }
        let handle = match resolve_child(self.runner.as_ref(), handle) {
            Ok(h) => h,
            Err(refused) => return refused,
        };
        let handle = handle.as_str();
        match self.runner.send(handle, text) {
            Ok(said) => Invocation::ok(said),
            // A refusal comes back unchanged, like `job_kill`'s: the runner is the only thing
            // that knows whether the child heard it, and a tool that reformatted its refusal
            // into a success is the defect this tree already paid for once.
            Err(why) => Invocation::failed(format!("subagent `{handle}` was not messaged"), why),
        }
    }
}

/// `task_resume` — bring back a subagent a daemon restart left unfinished, in ONE call.
///
/// The operator, 2026-10-12: *"if some threshold passed - say more than 1 hours after the stop,
/// we start to simply remind the model stating that time passed and this is what you wre running
/// and how to restart that"*. The reminder names this call; it resumes the child in its own
/// session (same id, same transcript, same worktree and branch, same role and model), continues
/// its turn with a restart notice, and its result reaches this session the way any child's does.
/// It is also the door the daemon's own automatic restore goes through.
pub struct TaskResumeTool {
    runner: Arc<dyn TaskRunner>,
}

impl TaskResumeTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskResumeTool { runner }
    }
}

impl Tool for TaskResumeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "task_resume",
            "Bring back a subagent that a daemon restart left unfinished. Give `task` (its \
             handle, which is its session id). It resumes in its own session — same \
             conversation, worktree, branch, role and model — and carries on from where it \
             was; collect it with `task_result` as before. Refused by name for a child that is \
             running, finished, or that this session did not start.",
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The handle of the subagent to bring back, or its slug."}
                },
                "required": ["task"]
            }),
            // `Session`, as `task` is: it restarts a child session of this one.
            Access::Session,
        )
    }

    fn invoke(&self, _ctx: &mut InvokeCtx<'_>, args: &serde_json::Value) -> Invocation {
        let Some(handle) = args.get("task").and_then(|v| v.as_str()) else {
            return Invocation::failed(
                "task_resume needs the `task` handle",
                "call `task_resume` with `task` set to the subagent's handle.",
            );
        };
        let handle = match resolve_child(self.runner.as_ref(), handle) {
            Ok(h) => h,
            Err(refused) => return refused,
        };
        let handle = handle.as_str();
        match self.runner.resume(handle) {
            // **`Backgrounded`, as `task`'s is** — the child is working again and has not
            // answered, and the outcome is what arms this session's watcher for its result.
            Ok(said) => Invocation::backgrounded(
                handle.to_string(),
                std::time::Duration::ZERO,
                letibot_transcript::Backgrounding::Asked,
                format!(
                    "{} `task_result` with task=\"{handle}\" shows where it has got to.",
                    super::DELIVERED
                ),
                said,
            ),
            Err(why) => Invocation::failed(format!("subagent `{handle}` was not resumed"), why),
        }
    }
}

#[cfg(test)]
mod tests;
