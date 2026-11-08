//! `task`: spawn a subagent and collect its answer; starting it is not running it.
//! A child inherits its parent's ruleset and `access` may narrow it, never widen it; `where`
//! picks the substrate and changes no permission. The runner is a seam whose default refuses.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

// The child's state and its evidence come from the one judgement both doors read, so
// `task_result` and the idle plan-check cannot answer differently about one child.
use crate::builtins::todo::{ChildDoing, RunningChild, child_doing, child_evidence};
use crate::exec::perimeter::{PerimeterGrant, SUBAGENT, Secret, TOP_LEVEL, Widen, WidenItem};
use crate::runtime::{Invocation, InvokeCtx, Tool};
pub use crate::schema::Downgrade;
use crate::schema::{Access, ToolSchema};

/// Where a subagent runs.
/// Not `Copy`: a placement may carry a host name, and a name is a `String`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Placement {
    /// The parent's own boundary, whatever it is.
    #[default]
    Host,
    /// A firecode VM. Which hypervisor is the backend's business.
    Firecode,
    /// Another machine, over ssh (`where: "host:lab2x1"`). The name is one the operator
    /// declared in the hosts file, checked against it at the call.
    Remote(String),
    /// A firecode VM on that machine (`--host NAME --vm`, `where: "host:NAME:vm"`): the
    /// tools run in a guest booted by the host's own `firecode`, so the boundary is a VM.
    RemoteVm(String),
}

/// The handle's `seam:` line for a placement: what the child's work is made of and where it
/// comes back. Empty for the host, whose work is the worktree itself.
fn placement_seam(placement: &Placement) -> String {
    match placement {
        // No branch on this path. The VM works on a copy, and firecode delivers the copy's
        // final state as a directory beside it when the VM comes down - the old line promised a
        // branch, which a reader then went looking for (FIRECODE-NOTES).
        Placement::Firecode => {
            "  seam: the VM works on a COPY of the tree; its work comes back as \
             a directory beside that copy when the VM comes down, not as this branch — diff \
             it and take what you want.\n"
                .to_string()
        }
        // A branch, and a true one: the remote works on a checkout, so its commits come back
        // by `git fetch`, and the line names the branch so the parent need not guess it.
        Placement::Remote(host) => format!(
            "  seam: the child works on {host} in its own checkout of this repository; what it \
             commits comes back here as the branch `remote/{host}/<its short id>` when it ends. \
             Uncommitted remote edits stay there.\n"
        ),
        // The guest is a boundary, but its tree is a copy on the host, so the checkout the
        // branch comes home from is the host's and a commit has to leave the guest first.
        Placement::RemoteVm(host) => format!(
            "  seam: the child works inside a firecode VM on {host}, on a guest copy of its \
             checkout; what it commits comes back here as the branch `remote/{host}/<its short \
             id>` when the VM stops. Uncommitted edits inside the guest stay there.\n"
        ),
        Placement::Host => String::new(),
    }
}

/// The `where` argument, refused when it cannot be honoured here: nothing is spawned and the
/// refusal names `host` as the way on. `firecode` is the CLI to look for when the caller names
/// one; the hosts are passed in so a test can say which file.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn placement_arg(
    args: &serde_json::Value,
    firecode: Option<&std::path::Path>,
    hosts: &crate::remote::Hosts,
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
    if let Some(name) = placement.host()
        && hosts.get(name).is_none()
    {
        return Err(Invocation::failed(
            "this box has no such host",
            format!(
                "{}, so `where: host:{name}` cannot be honoured. Nothing was spawned. A \
                 remote child needs a machine the operator declared; leave `where` out to run \
                 it in your own boundary.",
                hosts.refusal(name)
            ),
        ));
    }
    Ok(placement)
}

/// The `verify` argument: an acceptance check firecode runs after a VM-placed child finishes
/// ([`TaskSpec::vm_verify`]). Refused off a VM, where nothing would run it - taking it silently
/// would leave the caller believing in a gate that does not exist.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn verify_arg(
    args: &serde_json::Value,
    placement: &Placement,
) -> Result<Option<String>, Invocation> {
    let Some(v) = args
        .get("verify")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    if *placement != Placement::Firecode {
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
    /// `host`, `firecode`, `host:NAME`, or `host:NAME:vm` for a VM on that machine. The name
    /// keeps its case, a hostname not being a keyword, while keywords are matched
    /// case-insensitively because that is how a model writes them.
    pub fn parse(s: &str) -> Result<Placement, String> {
        let trimmed = s.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(name) = lower
            .find(':')
            .filter(|i| matches!(&lower[..*i], "host" | "remote"))
            .map(|i| trimmed[i + 1..].trim())
        {
            if name.is_empty() {
                return Err(
                    "`host:` needs a name — `where: \"host:lab2x1\"`, one of the hosts in \
                     ~/.config/letibot/hosts.toml"
                        .to_string(),
                );
            }
            // `host:NAME:vm` is a VM ON that machine: `--host NAME --vm` writes it and
            // `Placement::as_str` reads it back, so the two spellings are one fact.
            let base = name.len().saturating_sub(3);
            if name.len() > 3
                && name.is_char_boundary(base)
                && name[base..].eq_ignore_ascii_case(":vm")
            {
                let named = name[..base].trim();
                if named.is_empty() {
                    return Err("`host::vm` needs a host name between the colons".to_string());
                }
                return Ok(Placement::RemoteVm(named.to_string()));
            }
            return Ok(Placement::Remote(name.to_string()));
        }
        match lower.as_str() {
            "" | "host" | "here" => Ok(Placement::Host),
            "firecode" | "vm" | "firecracker" | "libvirt" => Ok(Placement::Firecode),
            other => Err(format!(
                "`{other}` is not a placement. There are four: `host` (the default), \
                 `firecode` (a VM), `host:NAME` (one of the machines the operator declared in \
                 ~/.config/letibot/hosts.toml), and `host:NAME:vm` (a VM on that machine)"
            )),
        }
    }

    /// The spelling that round-trips through [`Placement::parse`] - a recorded subagent's
    /// spec is written with this and read back with that, so the two are one fact.
    pub fn as_str(&self) -> String {
        match self {
            Placement::Host => "host".to_string(),
            Placement::Firecode => "firecode".to_string(),
            Placement::Remote(name) => format!("host:{name}"),
            Placement::RemoteVm(name) => format!("host:{name}:vm"),
        }
    }

    /// Is this placement a firecode VM, on this box or on a host, so the question is
    /// answered once rather than spelled as a `match` in each caller.
    pub fn is_vm(&self) -> bool {
        matches!(self, Placement::Firecode | Placement::RemoteVm(_))
    }

    /// The machine this placement is on, when it is another one.
    pub fn host(&self) -> Option<&str> {
        match self {
            Placement::Remote(name) | Placement::RemoteVm(name) => Some(name),
            _ => None,
        }
    }
}

/// What one `task` call asked for, beyond the prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSpec {
    pub role: String,
    pub downgrade: Downgrade,
    pub placement: Placement,
    /// Which model the child runs on. `None` inherits the parent's, `Some("local")` is the
    /// daemon's own server, and `Some("provider/model")` a cloud preset resolved through the
    /// same door `/models` uses, so a name a person can type is a name a child can run on.
    pub model: Option<String>,
    /// Where the child's files live, when `task_start` arranged a tree for it. `None` is a
    /// plain `task` call working in the parent's workspace; `Some` is a worktree the runner
    /// created before the spawn. See [`WorktreePlacement`] for why it rides the spec.
    pub worktree: Option<WorktreePlacement>,
    /// The tree a `firecode` placement copies, when the caller knows one the default resolution
    /// would not name. `None` resolves the child's workspace to the repository it is about, and a
    /// VM that copied one tree while the record says another lies silently.
    pub vm_source: Option<PathBuf>,
    /// The acceptance check a VM-placed child is held to: a shell command firecode runs after the
    /// child finishes and before the VM stops, whose exit status is the verdict. Without it a
    /// placed child's "done" is its own word. `None` on the host: no VM to run it in.
    pub vm_verify: Option<String>,
    /// The child's name, its slug, when the caller has one to give: a child named by its session id
    /// rings no bell. `None` is the runner derives it, which knows the session's other children and
    /// so makes the name unique among them, as [`unique_slug`] does for a worktree.
    pub slug: Option<String>,
    /// A tool's own helper, not the model's child: `digest`'s part readers. The tool that starts
    /// one waits for it and reads its answer ([`TaskRunner::consumed`]), so the model never gets
    /// its handle, and a restart neither resumes nor re-delivers it.
    pub helper: bool,
    /// The skills this child is started with, their bodies preloaded into its prompt on top of
    /// the index every session gets. Names are checked before the child exists, so a typo is a
    /// refusal. Empty means none of its own, which for a child is its parent's list.
    pub skills: Vec<String>,
    /// The `[perimeter.<name>]` table this child is started on, when the caller named one. `None`
    /// is the `subagent` table: the level's default, narrowed by whatever the project wrote.
    pub perimeter: Option<String>,
    /// What the operator GRANTED for this spawn beyond the parent's perimeter, and the reason it
    /// gave. Empty is the ordinary case, and a spawn's own answer lands on this child alone.
    pub widen: Vec<crate::exec::perimeter::WidenItem>,
    pub widen_because: Option<String>,
}

/// What one `task_start` call asked for, beyond the prompt and the [`TaskSpec`]: the slug, the base
/// the branch is cut from, and whether the main checkout was asked for. The runner answers with a
/// [`WorktreePlacement`]: the request is what the caller typed, the placement is what is on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeSpec {
    /// The slug the worktree and branch are named by. Empty is *derive it from the
    /// prompt* - the default, and the one [`slug_from_prompt`] answers.
    pub slug: String,
    /// The base ref the branch is cut from. `None` is the repo's current HEAD, which is
    /// the default and the one a caller that did not look at the branch gets.
    pub base: Option<String>,
    /// The exception: work in the main checkout rather than a fresh worktree.
    /// `false` is the default - a `task_start` that did not name the main tree creates
    /// a worktree, and the main tree is the thing it exists to keep the child out of.
    pub main_tree: bool,
    /// The repository to cut the worktree from, when it is not the one the session root is in:
    /// a session may run a level above its repositories, and then the root is in none. Relative
    /// to the session root, or absolute. `None` is the session root's own repository.
    pub repo: Option<String>,
}

/// Where a `task_start` child works, as the runner arranged it: the path, the branch and the base
/// SHA. It rides the [`TaskSpec`] rather than a second argument to [`TaskRunner::start`], because
/// the spawn is where a spec becomes a session, and a session elsewhere lies about its provenance.
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
    /// Whether the child works in the main checkout - the exception, named in the
    /// answer so the operator sees it rather than finds it.
    pub main_tree: bool,
}

/// The handle a `task_start` call returns, with the placement it arranged: a plain `task` returns a
/// bare handle, since the child works where the parent works. The placement is the point, a handle
/// that does not say where the child is leaving the operator to go and find it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHandle {
    /// The handle `task_result` collects by.
    pub handle: String,
    /// Where the child works, as the runner arranged it.
    pub placement: WorktreePlacement,
}

/// The slug a `task_start` child's worktree and branch are named by, derived from the prompt when
/// the caller did not name one. Deterministic in the prompt on purpose: the same subtask asked
/// twice gets the same slug, which is what makes refusing an existing path meaningful.
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

/// Why a slug the caller named cannot name a worktree and a branch, or `None`: the slug becomes
/// a directory and a ref, and a trimmed one let `/` nest, `..` escape the worktrees folder and a
/// space make a branch git refuses after the directory already existed.
pub fn slug_refusal(slug: &str) -> Option<String> {
    let why = if slug.contains("..") {
        "it contains `..`, which would leave the worktrees folder"
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

/// The first free slug from `base`: `base`, then `base-2`, `base-3`, where `taken` says whether
/// one is used. A derived slug collides whenever two briefs begin alike, and being nobody's
/// choice it yields to the next free one rather than refusing the spawn.
pub fn unique_slug(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..1000)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken(s))
        .unwrap_or_else(|| format!("{base}-{}", std::process::id()))
}

/// One child, by the names it answers to: its handle, and its slug, the name [`TaskSpec::slug`]
/// gave it. `own` says whether THIS session started it: a handle list is shared with the tree, so a
/// name is looked for among its own children first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildName {
    pub handle: String,
    pub slug: Option<String>,
    pub own: bool,
    /// No `subagent_run` row was ever written for this child, its spawn having lost the store to
    /// another writer or predated its first record, and how far it got: a lost child has no answer
    /// and nothing that resumes it, so the listing says so.
    pub lost: Option<LostChild>,
}

/// How far a child with no record got: the class both the listing and the single-handle answer
/// read, so a parent can tell a child that died mid-`bash` from one that never opened. The
/// judgement is the runner's, since the tools crate cannot see a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostChild {
    /// Its last row is an answer - an `Assistant` with no call after it. The text a reader
    /// gets IS that row, and nothing recorded a settlement, so it is
    /// [`NOT_A_FINISHED_ANSWER`].
    Answered,
    /// It stopped before an answer, and the words say where - `before it wrote a single row`,
    /// `mid-bash, on a call it never got an answer to`, `after bash's result`. They are
    /// `subagent_record::EndedIn::brief`'s, and they read on from *"it stopped "*.
    Stopped(String),
}

/// What the text of a child with no record is, in one sentence: the caveat both doors carry, so a
/// parent is never handed a turn that stopped mid-way under the name of a finished answer. Nothing
/// here may say the child finished or guess why the row is missing.
pub const NOT_A_FINISHED_ANSWER: &str =
    "whatever it had written when it stopped, not a finished answer";

impl ChildName {
    /// `fix-login · s-…-sub-3`, or the bare handle for a child nobody named - the job rule
    /// ([`crate::exec::job_label`]), so a child and a job are named the same way in every
    /// sentence about them.
    pub fn label(&self) -> String {
        subagent_label(self.slug.as_deref(), &self.handle)
    }
}

/// The one spelling of a child's name in a sentence: `slug · handle`, or the handle alone, the same
/// function a job's label is ([`crate::exec::job_label`]). The full handle stays, since it is what
/// the model collects by.
pub fn subagent_label(slug: Option<&str>, handle: &str) -> String {
    crate::exec::job_label(slug, handle)
}

/// A label split back into its name and handle, the inverse of [`subagent_label`], for a reader
/// of a sentence written either way. Only an `s-` handle on the right is a child's, so a label
/// that merely contains a `·` is not misread.
pub fn split_subagent_label(label: &str) -> (Option<&str>, &str) {
    match label.rsplit_once(" · ") {
        Some((slug, handle)) if handle.starts_with("s-") && !slug.trim().is_empty() => {
            (Some(slug), handle)
        }
        _ => (None, label),
    }
}

/// Which child the caller meant, over a list of children in hand. Pure, so the rules are assertable
/// without a runner: the handle wins, and a name that fits more than one child is not a name. A
/// name is looked for among this session's own children first, then in the rest of the tree.
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

/// A child, from what the caller wrote: its handle, or its slug. A name two children carry is
/// refused, naming them; a string that is neither is handed on unchanged, so the runner's refusal
/// names it as asked and `task_message` may address a peer.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
pub fn resolve_child(runner: &dyn TaskRunner, asked: &str) -> Result<String, Invocation> {
    resolve_child_in(&runner.children(), asked)
}

/// [`resolve_child`] over a list already in hand.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
pub fn resolve_child_in(children: &[ChildName], asked: &str) -> Result<String, Invocation> {
    match pick_child(children, asked) {
        PickedChild::One(handle) => Ok(handle),
        PickedChild::Unknown => Ok(asked.trim().to_string()),
        PickedChild::Ambiguous(many) => Err(ambiguous_child(asked, &many)),
    }
}

/// The refusal for a name more than one child carries. It names the child the way it was asked
/// for - the job rule - and lists the candidates by their labels, so the next call can use the
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

/// The name a plain `task` child gets: the `slug` it was given, or the one
/// [`slug_from_prompt`] derives, made unique among the names `taken` already holds (`-2`, `-3`,
/// …) - a child's name is a handle, and a name two children answer to resolves to neither.
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

/// The path a `task_start` worktree is created at, from the repo's workspace and the slug:
/// `<workspace>/.claude/worktrees/agent-<slug>`, the shape the operator's own worktrees take. Pure,
/// so tool and runner compute one path, a reported path the runner did not create being a claim.
pub fn worktree_path(workspace: &str, slug: &str) -> String {
    match worktree_home(std::path::Path::new(workspace)) {
        Some(home) => format!("{}/agent-{slug}", home.display()),
        None => format!("{workspace}/.claude/worktrees/agent-{slug}"),
    }
}

/// Where a repository's worktrees live, when its layout says: a repository in a folder of its own
/// name keeps them beside it, and one in an org folder keeps them in the org's `worktrees`, one
/// folder per repository. `None` keeps `.claude/worktrees` in the repository.
pub fn worktree_home(repo: &std::path::Path) -> Option<std::path::PathBuf> {
    let name = repo.file_name()?;
    let parent = repo.parent()?;
    if parent.file_name() == Some(name) {
        return Some(parent.join("worktrees"));
    }
    let repos = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .filter(|e| e.path().join(".git").is_dir())
        .count();
    (repos >= 2).then(|| parent.join("worktrees").join(name))
}

/// The branch a `task_start` worktree is cut onto, from the slug: `agent/<slug>`, named for what
/// it is, an agent's work cut from the base the caller named and to be landed by a merge queue
/// rather than pushed. See [`TaskStartTool`] for that rule.
pub fn worktree_branch(slug: &str) -> String {
    format!("agent/{slug}")
}

/// The rule a worktree child is given: the branch is the deliverable, so the work goes onto it and
/// the finished tree is clean. The daemon checks the same at turn end. An inherited cargo config
/// rides along: see [`cargo_patch_note`].
pub fn worktree_brief(path: &str, branch: &str) -> String {
    worktree_brief_with(path, branch, None)
}

/// [`worktree_brief`] with a `brief_` layer's added words (`docs/plugin-extension-points-wide.md`
/// X19). The placement and the deliverable rule are the host's and cannot be moved by a layer: a
/// brief that lost *the branch is the deliverable* is a child whose work nobody can land.
pub fn worktree_brief_with(path: &str, branch: &str, brief_add: Option<&str>) -> String {
    let mut said = format!(
        "[the deliverable] You work in the worktree `{path}` on branch `{branch}`, and the \
         branch is the deliverable: commit your work to it as you go and before you finish. \
         A finished child's worktree is clean (`git status --porcelain` prints nothing), and \
         your final answer names the commit(s)."
    );
    if let Some(note) = cargo_patch_note(std::path::Path::new(path)) {
        said.push_str("\n\n");
        said.push_str(&note);
    }
    if let Some(add) = brief_add.map(str::trim).filter(|a| !a.is_empty()) {
        said.push_str("\n\n");
        said.push_str(add);
    }
    said
}

/// A cargo config above the tree that patches a crate. Cargo reads `.cargo/config.toml` upwards,
/// but the merge queue's landing tree lies outside the repository, so a build that passes here can
/// fail there. `None` when nothing above the tree patches anything.
pub fn cargo_patch_note(worktree: &std::path::Path) -> Option<String> {
    let start = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    for dir in start.ancestors().skip(1) {
        for name in [".cargo/config.toml", ".cargo/config"] {
            let path = dir.join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let patched = patched_crates(&text);
            if patched.is_empty() {
                continue;
            }
            return Some(format!(
                "[the build here is not the build the merge queue runs] Cargo reads \
                 `.cargo/config.toml` from a tree's parents, and `{}` replaces {} with a path of \
                 its own. The queue's landing tree is made outside this repository and has no such \
                 file, so the gate builds what `Cargo.toml` says. Before you call this done, check \
                 the change against that build rather than this one.",
                path.display(),
                match patched.as_slice() {
                    [one] => format!("`{one}`"),
                    many => format!("{} crates (`{}`)", many.len(), many.join("`, `")),
                }
            ));
        }
    }
    None
}

/// The crates a config's `[patch…]` sections replace. Comments are not sections, and a section runs
/// to the next header, so `[build]` after a patch is not read as one.
fn patched_crates(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_patch = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_patch = line.starts_with("[patch");
            continue;
        }
        if in_patch && let Some((key, _)) = line.split_once('=') {
            let key = key.trim().trim_matches('"');
            if !key.is_empty() && !out.iter().any(|seen| seen == key) {
                out.push(key.to_string());
            }
        }
    }
    out
}

/// Where a spawned subagent has got to: the variant is the child's first turn's record, `child` is
/// its present, and the difference is what lets a parent tell a child working from one parked
/// between turns from one wedged mid-call. `None` means this runner keeps no such record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Still working - the child's turn has not ended. `note` is the daemon's own progress line
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
        /// The child's present, which the answer is not. `Some` with `working == false` is a
        /// child that is still HERE and between turns - the state whose only door is a message, and
        /// the one a parent could not see before this field.
        child: Option<RunningChild>,
    },
    /// It did not finish, and this is why.
    Failed { why: String },
    /// No subagent by that name was ever spawned here.
    Unknown,
}

impl TaskStatus {
    /// Which of the states the child is in, or `None` when this runner keeps no reading - the
    /// one word a listing row and the full sentence both start from.
    pub fn doing(&self) -> Option<ChildDoing> {
        match self {
            TaskStatus::Running { child, .. } | TaskStatus::Done { child, .. } => {
                child.as_ref().map(child_doing)
            }
            TaskStatus::Failed { .. } | TaskStatus::Unknown => None,
        }
    }

    /// The child's present as one clause: the state, the evidence, and the move that state asks
    /// for, or `None` when there is no reading to give. The state and its evidence are the tools
    /// crate's one judgement; only the move differs, and here it names `task_message`.
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
            // The idle fix. A parked child is not stuck and not working; the one door that
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

/// What a finished `task_start` child's branch became, the answer to [`TaskRunner::finished`]. The
/// tool layer does not know what a merge queue is, and that is the point: what it needs is the one
/// sentence a person reads, plus a refusal when the enqueue could not be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    /// The child's branch was put in the queue for landing: this is the entry, or, when the finish
    /// was noticed twice, the same entry it already had. The two are one value because they are one
    /// fact, and the id is carried so a reader can find the row.
    Queued {
        /// The entry's id, which is the child's own handle.
        id: String,
        /// The branch the entry is about.
        branch: String,
    },
    /// There is no branch to land, and this is why - a plain `task` child works in the
    /// parent's own tree, and a `task_start` in the main checkout works on main itself.
    NoBranch { why: String },
}

/// Starts subagents and collects them. Starting is not running: a round's calls run in order on one
/// thread, so a blocking start would hold every call behind it for as long as the child lived,
/// which reads as a hang. A subagent is minutes of work, so starting one returns a handle.
pub trait TaskRunner: Send + Sync {
    /// Start the subtask and return the handle it can be collected by. Returns
    /// when the child is spawned, not when it is finished.
    fn start(&self, prompt: &str, spec: &TaskSpec) -> Result<String, String>;

    /// Arrange the tree, then start the child: the `task_start` door. The order is the difference
    /// from [`Self::start`], an existing path being refused here rather than by a child that never
    /// started; a runner that cannot arrange a tree refuses by name, as [`Self::kill`] does.
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

    /// Where a child has got to, for a READER (`task_result`): [`Self::collect`] by default, and a
    /// runner that records children answers from its record for one a restart left without a slot,
    /// so the listing, the single answer and `task_resume` read one state.
    fn look(&self, handle: &str, timeout: std::time::Duration) -> TaskStatus {
        self.collect(handle, timeout)
    }

    /// Whether this child has no record, and how far it got: `None` for a recorded child and for a
    /// runner that keeps no store. A keyed question, because the listing carries only the children
    /// a row was due for, while a model may name any of its children.
    fn lost(&self, _handle: &str) -> Option<LostChild> {
        None
    }

    /// This child's settlement has been read, by `task_result` or by the tool that started a
    /// helper. A runner that records children marks the record delivered, so a restart does not
    /// hand the same answer over as news. A runner that records nothing has nothing to mark.
    fn consumed(&self, _handle: &str) {}

    /// The handles this session has started, oldest first - what `task_result`
    /// lists when it is called with no handle, so a model that lost one can find
    /// it without the operator.
    fn started(&self) -> Vec<String> {
        Vec::new()
    }

    /// The children a handle or a name may resolve to, with their slugs: what [`resolve_child`]
    /// reads and what the sentences about a child label it by. The default is [`Self::started`]
    /// with no names, so a runner that keeps none resolves handles as before.
    fn children(&self) -> Vec<ChildName> {
        self.started()
            .into_iter()
            .map(|handle| ChildName {
                handle,
                slug: None,
                own: true,
                lost: None,
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

    /// The session that started this one: the address a child speaks to its parent by, under
    /// [`PARENT_HANDLE`]. The upward edge lacked an address, not a door, since `task_message` takes
    /// any live id but nothing told a child the one it cannot guess.
    fn parent(&self) -> Option<String> {
        None
    }

    /// `slug · handle` for one child, or the handle - [`subagent_label`] over [`Self::slug_of`].
    fn label_of(&self, handle: &str) -> String {
        subagent_label(self.slug_of(handle).as_deref(), handle)
    }

    /// A `task_start` child has finished: its branch goes to be landed, the seam the merge queue's
    /// enqueue hangs on, and it is the runner and not the tool, which has no brief, no placement
    /// and no store. `NoBranch` is the ordinary answer for a plain `task`; `Err` is a real failure.
    fn finished(&self, handle: &str) -> Result<Finished, String> {
        let _ = handle;
        Ok(Finished::NoBranch {
            why: "this session's runner arranges no worktree, so a child of it leaves no \
                  branch to land"
                .into(),
        })
    }

    /// Stop a subagent. `Ok` is what was done, in the words the operator gets; `Err` is why it
    /// could not be, said rather than swallowed. A subagent is not a process, so the kill is an
    /// interrupt of the child's turn, the only thing it is doing.
    fn kill(&self, handle: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot stop `{handle}`: a subagent is stopped by \
             interrupting the turn it is running, which only the runner that started it \
             can do. Nothing was stopped."
        ))
    }
    /// Say something to a session that is still working, or to a peer: the handle may name any live
    /// session. A message is not a second prompt, being an agent's utterance, so it authorises
    /// nothing. A running turn hears it at its next boundary.
    fn send(&self, handle: &str, _text: &str) -> Result<String, String> {
        Err(format!(
            "this session's runner cannot message `{handle}`: a subagent is reached through \
             the turn it is running, which only the runner that started it can do. Nothing \
             was sent."
        ))
    }

    /// Stop every child this session still owns: a parent being stopped stops its children first,
    /// so the tree never leaves work computing for nobody. The children, not the subtree, so the
    /// walk down is one level per session, one result each.
    fn stop_all(&self) -> Vec<(String, Result<String, String>)> {
        Vec::new()
    }

    /// Wait until every child this session started has left its own thread, up to `timeout`; `true`
    /// when they all have. A child's thread is spawned detached, so nothing can tell one that has
    /// finished from one still running, and a thread still inside `libllama` at exit is a SIGSEGV.
    fn wait_for_children(&self, _timeout: std::time::Duration) -> bool {
        true
    }

    /// How many children this session started still have a thread, the ones [`Self::stop_all`]
    /// would stop. A session with any is not idle, whatever its own queue says: a child it waits on
    /// can settle at any moment and must find its parent's reader there.
    fn live_children(&self) -> usize {
        0
    }

    /// How many of this session's children have not answered yet - still on their task, as
    /// opposed to answered and idle. A child with any is not done (the operator's ruling): its
    /// answer is held until they have. Zero for a runner that keeps no slots.
    fn unsettled_children(&self) -> usize {
        0
    }

    /// Bring back the children a daemon that is gone left running, called once when this session is
    /// opened. [`RestoreMode::Resume`] brings each back, [`RestoreMode::Remind`] only reports it,
    /// so the model can resume it itself. A runner that records nothing has nothing to bring back.
    fn resume_recorded(&self, _mode: RestoreMode) -> Vec<RestartedChild> {
        Vec::new()
    }

    /// Bring back ONE child a daemon that is gone left unfinished - the `task_resume` door,
    /// and the same door the automatic restore goes through. `Ok` is the sentence for the model;
    /// `Err` is why it was not brought back, with nothing spawned.
    fn resume(&self, handle: &str) -> Result<String, String> {
        Err(format!(
            "no subagent runner that records children is installed here, so `{handle}` cannot \
             be resumed. Nothing was spawned."
        ))
    }
}

/// Whether a restart brings children back, or only says what they were doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    /// The daemon was down a short while: bring each mid-task child back.
    Resume,
    /// It was down long enough that the world may have moved on: report each child and how to
    /// bring it back with `task_resume`, and leave the decision to the model.
    Remind,
}

/// What a parent's open did with one child a previous daemon left behind - see
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
    /// model to bring back (`task_resume`) - where it was working, so the reminder can say.
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
    /// The firecode CLI a `where: firecode` call is checked against, when the caller names one;
    /// `None` is the host's answer ([`crate::firecode::firecode_available`]). A field and not an
    /// environment variable, which a test setting it would race against every other reader.
    firecode_bin: Option<std::path::PathBuf>,
    /// The machines `where: "host:NAME"` may name. `None` is [`crate::remote::Hosts::load`], which
    /// reads the operator's file, the only place a host may be declared. A field so a test says
    /// which file without an environment variable that would race the other readers of it.
    hosts: Option<crate::remote::Hosts>,
    /// The skills corpus a call's `skills` names are checked against. `None` in a registry a
    /// test built by hand: a call that names one is then refused rather than spawned with a
    /// list nothing could vouch for.
    skills: Option<Arc<crate::builtins::skill::SkillRegistry>>,
}

impl TaskTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskTool {
            runner,
            firecode_bin: None,
            hosts: None,
            skills: None,
        }
    }

    /// Read a call's `skills` against `skills` rather than refusing the argument outright.
    pub fn with_skills(mut self, skills: Arc<crate::builtins::skill::SkillRegistry>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Check `where: firecode` against `bin` rather than the host's `$FIRECODE_BIN`/PATH.
    pub fn with_firecode_bin(mut self, bin: impl Into<std::path::PathBuf>) -> Self {
        self.firecode_bin = Some(bin.into());
        self
    }

    /// Read `where: "host:NAME"` against `hosts` rather than the operator's file.
    pub fn with_hosts(mut self, hosts: crate::remote::Hosts) -> Self {
        self.hosts = Some(hosts);
        self
    }
}

/// The `wake` argument's description, the same for `task` and `task_start` (`task_start`
/// had none, while its result promised a delivery `wake` decides).
const WAKE_ARG: &str = "`later` says you do not need waking for it: the answer is shown at your \
     next turn and a `wait` does not hold on it. `now`: the moment it finishes, even mid-turn. \
     Absent (the default): its end wakes you.";

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
             the repository's own format check (`sh scripts/check-fmt.sh <the ref it was cut from>` in this tree)**: code a child has \
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
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen. **`read-only` removes `bash`, so a child asked to review a branch cannot run `git status` or `git diff` and judges from the files alone — for a reviewer pass `no-write` instead**, which keeps the shell and takes only the pen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary), `firecode` (a VM: a copy of this workspace, permission checks off inside, so a child there asks nobody), or `host:NAME` — a machine the operator declared for this box, where the child's tools (bash, read, write, jobs) run over ssh while its model connection stays here. Call `hosts` to see which machines are declared, and `hosts {probe: true}` to see which is least loaded. Placement never changes permissions."},
                    "verify": {"type": "string", "description": "With `where: firecode`: a shell command firecode runs in the child's project AFTER the child finishes and before its VM stops - the e2e or test command that says whether the work is done. Its exit status is the verdict, reported in the child's answer with its log; the child is told nothing it can fake. Refused without `where: firecode`."},
                    "repo": {"type": "string", "description": "With `where: firecode`: the repository the VM copies, relative to the session root or absolute. A VM holds one repository, so it is required when the session root holds several. Refused without `where: firecode`."},
                    "model": {"type": "string", "description": "Run the child on a different model than yours (`local`, or `PROVIDER/MODEL` — an unknown name or a missing key is refused at the spawn, naming the fix)."},
                    "slug": {"type": "string", "description": "A short name for the child (letters, digits, `-`), your word for the work — `task_result`, `task_message` and `job_kill` take it in place of the handle. Defaults to a derivation from the prompt; made unique among your subagents."},
                    "wake": {"type": "string", "enum": ["now", "later"], "description": WAKE_ARG},
                    "skills": {"type": "array", "items": {"type": "string"}, "description": "Skills this child is STARTED with: each body is preloaded into its prompt, on top of the index every session gets. A name that matches nothing is refused before the child starts."},
                    "perimeter": {"type": "string", "description": "The `[perimeter.<name>]` table this child is started on, from `leticode.toml`. A child starts from YOUR perimeter and may only narrow it; anything wider than you hold is the operator's answer and not yours, so ask with `widen`."},
                    "widen": widen_arg()
                },
                "required": ["prompt"]
            }),
            // `Session`, not `Exec`: `task` runs no host command itself, delegating to a child turn
            // whose own gate governs its calls. As `Exec` it would hit the rule that exec always
            // asks, and a subagent that could not be spawned without a head would never run.
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
        let placement =
            match placement_arg(args, self.firecode_bin.as_deref(), &hosts_of(&self.hosts)) {
                Ok(p) => p,
                Err(refused) => return refused,
            };
        let vm_verify = match verify_arg(args, &placement) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        // The repository a VM copies, when the model names one - only for a VM.
        let vm_repo = args
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|r| r.trim().trim_end_matches('/').to_string())
            .filter(|r| !r.is_empty() && r != ".")
            .map(std::path::PathBuf::from);
        if vm_repo.is_some() && placement != Placement::Firecode {
            return Invocation::failed(
                "`repo` names the repository a VM copies, and this child is not placed in one",
                "add `where: firecode`, or drop `repo` — a child on the host works in your own \
                 tree, and `task_start` takes `repo` for a worktree.",
            );
        }
        // When the child's answer reaches the model - refused by name before anything
        // spawns, as `bash` does.
        let wake = match super::bash::wake_arg(args) {
            Ok(w) => w,
            Err(refused) => return refused,
        };
        // The child's name, when the caller gave one: held to `task_start`'s rule for a slug,
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
        // The skills, checked before anything is spawned. A name nothing answers to would
        // otherwise be a child that boots without the instructions its parent named, which is
        // the silent-provenance defect this file refuses everywhere else.
        let skills = match args.get("skills").and_then(|v| v.as_array()) {
            None => Vec::new(),
            Some(items) => {
                let named: Vec<String> = items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let Some(registry) = &self.skills else {
                    return Invocation::failed(
                        "`skills` cannot be checked here",
                        "this registry seats no skill corpus, so no name can be vouched for and                          nothing was spawned. Drop `skills`.",
                    );
                };
                let unknown: Vec<&str> = named
                    .iter()
                    .filter(|n| registry.load_any(n).is_err())
                    .map(String::as_str)
                    .collect();
                if !unknown.is_empty() {
                    let here = registry
                        .index_rows()
                        .0
                        .iter()
                        .map(|r| r.name.clone())
                        .collect::<Vec<_>>();
                    return Invocation::failed(
                        format!(
                            "no skill named {} — the skills here are {}",
                            unknown.join(", "),
                            if here.is_empty() {
                                "none".to_string()
                            } else {
                                here.join(", ")
                            }
                        ),
                        "`skill` action=\"list\" shows every one; nothing was spawned.",
                    );
                }
                named
            }
        };
        // What this child sees, checked before anything is spawned: its perimeter is composed from
        // this session's own and may only narrow it, and anything wider is a card.
        let (perimeter, widen, widen_because) = match perimeter_args(ctx, args) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
            perimeter,
            widen,
            widen_because,
            model: args
                .get("model")
                .and_then(|v| v.as_str())
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
            // A plain `task` works in the parent's workspace: no worktree is arranged.
            worktree: None,
            // And a VM placement copies that workspace's own copy source, which is the repository
            // it is about whenever there is one to name, or the one the model NAMES with `repo`: a
            // VM holds one repository. The spawn refuses one that is not a repository.
            vm_source: vm_repo,
            vm_verify,
            slug,
            // A child the model started is the model's, never a tool's helper.
            helper: false,
            skills,
        };
        let handle = match self.runner.start(prompt, &spec) {
            Ok(h) => h,
            Err(e) => return Invocation::failed(e, "the subagent did not start."),
        };
        // The name the runner gave it, for every sentence below: `slug · handle`.
        let label = self.runner.label_of(&handle);
        ctx.progress(format!("subagent {label} started"));
        if let Some(w) = wake {
            ctx.want_completion(&handle, w);
        }
        // `Backgrounded`, not `Ok`: the outcome names a fact about the world, this is running and
        // has not answered yet, and an `Ok` here would be grounding for an answer nothing produced.
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
                    // The model is named when the child was given one - a spawn that
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

/// The `widen` argument, one object so the reason and the grants cannot arrive apart: a card
/// without a reason is a card nobody can answer.
fn widen_arg() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "What this child needs BEYOND your own perimeter, and why. A child starts \
                        from your perimeter and may only narrow it, so anything here that you do \
                        not already hold is put to the operator as a card - even at allow-all, \
                        because widening a perimeter is not a mode's call. Granted, it lands on \
                        that child only; its own children inherit at most it.",
        "properties": {
            "grants_ro": {"type": "array", "items": {"type": "string"}, "description": "Paths this child may read, `~/` or absolute."},
            "grants_rw": {"type": "array", "items": {"type": "string"}, "description": "Paths this child may write, `~/` or absolute."},
            "secrets": {"type": "array", "items": {"type": "string"}, "description": "Named secrets it may carry. The names are `github`."},
            "because": {"type": "string", "description": "The reason, in your words: what the child cannot do without this."}
        },
        "required": ["because"]
    })
}

/// What a spawn says about its child's perimeter: the `[perimeter.<name>]` table it names, the
/// grants that table does not cover, and the reason. A widening is asked of the operator HERE,
/// before anything is spawned, so a refusal spawns nothing at all.
#[allow(clippy::result_large_err, clippy::type_complexity)] // Invocation is the failed row the caller shows
fn perimeter_args(
    ctx: &InvokeCtx<'_>,
    args: &serde_json::Value,
) -> Result<(Option<String>, Vec<WidenItem>, Option<String>), Invocation> {
    let named = args
        .get("perimeter")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(n) = &named
        && n == TOP_LEVEL
    {
        return Err(Invocation::failed(
            format!("`{n}` is the top level's own perimeter, and a child is never on it"),
            "a child starts from your perimeter at the tight level and may only narrow it. Name \
             a `[perimeter.<name>]` table, or leave `perimeter` out. Nothing was spawned."
                .to_string(),
        ));
    }
    let Some(widen) = args.get("widen") else {
        return Ok((named, Vec::new(), None));
    };
    let list = |key: &str| -> Result<Vec<String>, Invocation> {
        let bad = || {
            Invocation::failed(
                format!("`widen.{key}` is a list of strings"),
                "nothing was spawned.",
            )
        };
        match widen.get(key) {
            None => Ok(Vec::new()),
            Some(v) => v
                .as_array()
                .ok_or_else(bad)?
                .iter()
                .map(|x| x.as_str().map(str::to_string).ok_or_else(bad))
                .collect(),
        }
    };
    let grants_ro = list("grants_ro")?;
    let grants_rw = list("grants_rw")?;
    let mut secrets = Vec::new();
    for name in list("secrets")? {
        let Some(s) = Secret::parse(&name) else {
            return Err(Invocation::failed(
                format!("`{name}` is not a secret this build knows"),
                format!(
                    "the names are: {}. Nothing was spawned.",
                    Secret::ALL
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        };
        secrets.push(s);
    }
    let because = widen
        .get("because")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let want = Widen {
        grants_ro,
        grants_rw,
        secrets,
        because: because.clone(),
        from: None,
    };
    if want.is_empty() {
        return Ok((named, Vec::new(), because));
    }
    let Some(because) = because else {
        return Err(Invocation::failed(
            "`widen` needs a `because`",
            "widening a perimeter is put to the operator as a card, and a card without a reason \
             is one nobody can answer. Nothing was spawned."
                .to_string(),
        ));
    };
    let Some(parent) = ctx.perimeter() else {
        return Err(Invocation::failed(
            "`widen` needs this session's own perimeter, and this runtime has none",
            "the daemon opens a session with the perimeter its config gives it; without one \
             nothing can be said to be wider. Nothing was spawned."
                .to_string(),
        ));
    };
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let items = parent.widening(&want, &home).map_err(|why| {
        Invocation::failed(
            "that widening is not one this build would grant",
            format!("{why}. Nothing was spawned."),
        )
    })?;
    if items.is_empty() {
        return Ok((named, items, Some(because)));
    }
    let child = named.clone().unwrap_or_else(|| SUBAGENT.to_string());
    let ask = parent.ask(&child, &items, Some(&because));
    match ctx.grant_perimeter(&ask) {
        PerimeterGrant::Granted => Ok((named, items, Some(because))),
        PerimeterGrant::Refused(why) => Err(Invocation::failed(
            "the operator refused this widening, so nothing was spawned",
            format!(
                "the child would have been WIDER than this session's `{}` perimeter: {}. The \
                 refusal, in their words: {why}",
                parent.name,
                items
                    .iter()
                    .map(|i| i.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
        PerimeterGrant::NotAsked => Err(Invocation::failed(
            "nobody could be asked whether this child may be wider than its parent",
            format!(
                "it asks for {} beyond this session's `{}` perimeter. Widening a perimeter is \
                 the operator's call and not a mode's, so it does not happen on a question \
                 nobody answered. Nothing was spawned.",
                items
                    .iter()
                    .map(|i| i.label())
                    .collect::<Vec<_>>()
                    .join(", "),
                parent.name
            ),
        )),
    }
}

/// The operator's hosts as this call sees them: the one a test handed in, or the file.
fn hosts_of(given: &Option<crate::remote::Hosts>) -> crate::remote::Hosts {
    match given {
        Some(h) => h.clone(),
        None => crate::remote::Hosts::load(),
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

/// `task_start`: arrange a subagent's placement before it spawns it. Placement lives in the tool,
/// not in the caller's memory: the worktree is created first, an existing path is refused by name,
/// and only then does the child start in the tree just made.
pub struct TaskStartTool {
    runner: Arc<dyn TaskRunner>,
    /// The firecode CLI a `where: firecode` call is checked against - see
    /// [`TaskTool::with_firecode_bin`].
    firecode_bin: Option<std::path::PathBuf>,
    /// The machines `where: "host:NAME"` may name - see [`TaskTool::with_hosts`].
    hosts: Option<crate::remote::Hosts>,
    /// The skills corpus a call's `skills` names are checked against - see
    /// [`TaskTool::with_skills`].
    skills: Option<Arc<crate::builtins::skill::SkillRegistry>>,
}

impl TaskStartTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskStartTool {
            runner,
            firecode_bin: None,
            hosts: None,
            skills: None,
        }
    }

    /// Check `where: firecode` against `bin` rather than the host's `$FIRECODE_BIN`/PATH.
    pub fn with_firecode_bin(mut self, bin: impl Into<std::path::PathBuf>) -> Self {
        self.firecode_bin = Some(bin.into());
        self
    }

    /// Read a call's `skills` against `skills` rather than refusing the argument outright.
    pub fn with_skills(mut self, skills: Arc<crate::builtins::skill::SkillRegistry>) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Read `where: "host:NAME"` against `hosts` rather than the operator's file.
    pub fn with_hosts(mut self, hosts: crate::remote::Hosts) -> Self {
        self.hosts = Some(hosts);
        self
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
             a fresh worktree named `agent-<slug>` — beside the repository in its \
             project's `worktrees` folder when it lives in one (`~/Projects/x/x`, or an org \
             folder of several), in `.claude/worktrees` otherwise — on branch \
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
                    "access": {"type": "string", "description": "Narrow the subagent below your own permissions: `read-only` (no write, exec or network), or any of `no-write`, `no-exec`, `no-network`, comma-separated. Omit to inherit yours unchanged. Cannot widen. **`read-only` removes `bash`, so a child asked to review a branch cannot run `git status` or `git diff` and judges from the files alone — for a reviewer pass `no-write` instead**, which keeps the shell and takes only the pen."},
                    "where": {"type": "string", "description": "`host` (the default: your own boundary), `firecode` (a VM: a copy of this workspace, permission checks off inside, so a child there asks nobody), or `host:NAME` — a machine the operator declared for this box, where the child's tools (bash, read, write, jobs) run over ssh while its model connection stays here. Call `hosts` to see which machines are declared, and `hosts {probe: true}` to see which is least loaded. Placement never changes permissions."},
                    "verify": {"type": "string", "description": "With `where: firecode`: a shell command firecode runs in the child's project AFTER the child finishes and before its VM stops - the e2e or test command that says whether the work is done. Its exit status is the verdict, reported in the child's answer with its log; the child is told nothing it can fake. Refused without `where: firecode`."},
                    "model": {"type": "string", "description": "Run the child on a different model than yours (`local`, or `PROVIDER/MODEL` — an unknown name or a missing key is refused at the spawn, naming the fix)."},
                    "base": {"type": "string", "description": "The ref the branch is cut from. Defaults to the repo's current HEAD."},
                    "slug": {"type": "string", "description": "The slug the worktree and branch are named by. Defaults to a derivation from the prompt."},
                    "main_tree": {"type": "boolean", "description": "Work in the main checkout instead of a fresh worktree. The exception; off by default."},
                    "repo": {"type": "string", "description": "The repository to work in, when it is not the one the session root is in (a session above its repositories names one). Relative to the session root, or absolute."},
                    "wake": {"type": "string", "enum": ["now", "later"], "description": WAKE_ARG},
                    "skills": {"type": "array", "items": {"type": "string"}, "description": "Skills this child is STARTED with: each body is preloaded into its prompt, on top of the index every session gets. A name that matches nothing is refused before the child starts."},
                    "perimeter": {"type": "string", "description": "The `[perimeter.<name>]` table this child is started on, from `leticode.toml`. A child starts from YOUR perimeter and may only narrow it; anything wider than you hold is the operator's answer and not yours, so ask with `widen`."},
                    "widen": widen_arg()
                },
                "required": ["prompt"]
            }),
            // `Session`, exactly as `task` is: this arranges a tree and delegates to a child turn
            // whose own gate governs its write, exec and network calls. The git work happens in the
            // runner; this layer has no git and no exec.
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
        let firecode = self.firecode_bin.as_deref();
        let placement = match placement_arg(args, firecode, &hosts_of(&self.hosts)) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let vm_verify = match verify_arg(args, &placement) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        // `task_start` takes `wake` as `task` does, refused by name before anything is
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

        // A VM child works on a COPY of the tree, so a worktree arranged here is not the tree its
        // commits land in. `main_tree` promises no branch, so it is not this case.
        if placement.is_vm() && !main_tree {
            // A guest on a declared host comes home as a remote branch, so the reason is that
            // branch and not "nothing to enqueue" - that sentence is this box's VM alone.
            let why = match &placement {
                Placement::RemoteVm(host) => format!(
                    "the child works inside a firecode VM on {host}, and what it commits comes \
                     back here as the branch `remote/{host}/<its short id>` when the VM stops - \
                     not into a worktree arranged here, so `agent/<slug>` would stay at its base"
                ),
                _ => "a VM works on a copy of this tree, so the child's commits stay in the copy \
                      and the branch `agent/<slug>` never leaves its base, with nothing to \
                      enqueue for review"
                    .to_string(),
            };
            return Invocation::failed(
                "task_start cannot arrange a worktree for a child in a VM",
                format!(
                    "{why}. Use `task` with the same `where`, whose completion says where the \
                     work comes back. Nothing was arranged and nothing was spawned."
                ),
            );
        }

        // The slug, derived from the prompt when the caller did not name one: the derivation is the
        // default, not a lock, so a caller wanting another name for the same work passes `slug`. A
        // named one is checked before anything is made.
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

        // The workspace, or the refusal that names its absence. A remote session's workspace is on
        // another machine, and this tool creates worktrees with the local `git`, so it refuses
        // rather than making one under a path that belongs there.
        if let Some(host) = ctx.backend.host_label() {
            return Invocation::failed(
                "task_start arranges a worktree on this machine, and this session's tools run \
                 elsewhere",
                format!(
                    "this session works on {host}, so a worktree made here would be a tree the \
                     child could not reach. Nothing was arranged and nothing was spawned. Use \
                     `task` — a child of a remote session runs on the same host and gets its \
                     own checkout there, on branch `remote/<its short id>`."
                ),
            );
        }
        let Some(workspace) = ctx.backend.workspace_path() else {
            return Invocation::failed(
                "task_start needs a workspace",
                "this session has no workspace to arrange a worktree in. Nothing was \
                 arranged and nothing was spawned.",
            );
        };

        // The refusal of an existing path, by name and without spawning anything: a path that
        // already exists, a directory or a worktree an earlier `task_start` made, is refused here
        // rather than by a child that never started.
        if !main_tree {
            // Under the named repository, when there is one - the runner arranges it there.
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

        // The skills, checked before anything is spawned. A name nothing answers to would
        // otherwise be a child that boots without the instructions its parent named, which is
        // the silent-provenance defect this file refuses everywhere else.
        let skills = match args.get("skills").and_then(|v| v.as_array()) {
            None => Vec::new(),
            Some(items) => {
                let named: Vec<String> = items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let Some(registry) = &self.skills else {
                    return Invocation::failed(
                        "`skills` cannot be checked here",
                        "this registry seats no skill corpus, so no name can be vouched for and                          nothing was spawned. Drop `skills`.",
                    );
                };
                let unknown: Vec<&str> = named
                    .iter()
                    .filter(|n| registry.load_any(n).is_err())
                    .map(String::as_str)
                    .collect();
                if !unknown.is_empty() {
                    let here = registry
                        .index_rows()
                        .0
                        .iter()
                        .map(|r| r.name.clone())
                        .collect::<Vec<_>>();
                    return Invocation::failed(
                        format!(
                            "no skill named {} — the skills here are {}",
                            unknown.join(", "),
                            if here.is_empty() {
                                "none".to_string()
                            } else {
                                here.join(", ")
                            }
                        ),
                        "`skill` action=\"list\" shows every one; nothing was spawned.",
                    );
                }
                named
            }
        };
        // And what it sees, checked before the tree is arranged: a refusal here leaves nothing.
        let (perimeter, widen, widen_because) = match perimeter_args(ctx, args) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        let spec = TaskSpec {
            role: role.to_string(),
            downgrade,
            placement,
            perimeter,
            widen,
            widen_because,
            model,
            // The runner fills this after the git work: the placement is a fact the
            // spawn carries, and the tool does not invent one the runner did not make.
            worktree: None,
            // A `task_start` works in the worktree it just arranged, so a VM placement copies
            // that - the child's own workspace, which is what `None` means.
            vm_source: None,
            vm_verify,
            // The child is named after its worktree - the slug its branch `agent/<slug>` is
            // cut from, already made unique against the repository above. A main-tree child has
            // no branch of its own and is still named by the same slug.
            slug: Some(slug.clone()),
            // A child the model started is the model's, never a tool's helper.
            helper: false,
            skills,
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
        ctx.progress(format!(
            "subagent {label} started in {}",
            handle.placement.path
        ));
        if let Some(w) = wake {
            ctx.want_completion(&handle.handle, w);
        }

        // The placement is in the answer: the handle names the path, the branch and the base SHA,
        // so the caller and the operator see where the child works without re-reading the brief.
        // The main tree is named as the exception, and a firecode spawn says the VM got a copy.
        let where_line = if handle.placement.main_tree {
            "the main checkout (the exception — a fresh worktree is the default)".to_string()
        } else {
            "a fresh worktree".to_string()
        };
        let seam_line = match handle.placement.main_tree {
            true => String::new(),
            false => placement_seam(&spec.placement),
        };
        // The build here is not the gate's build, when a cargo config above the tree says so:
        // said in the answer as well as in the brief, because it is the parent who reads this.
        let build_line = match handle.placement.main_tree {
            true => String::new(),
            false => match cargo_patch_note(std::path::Path::new(&handle.placement.path)) {
                Some(note) => format!("  {note}\n"),
                None => String::new(),
            },
        };
        // `Backgrounded`, not `Ok` - the same reason `task` gives: the outcome
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
                 branch: {}\n  base: {}\n{seam_line}{build_line}\nThe branch is the deliverable and \
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

/// What a seat's `task_result` and `task_message` may reach. The daemon's runner addresses any live
/// session by id, which is right for a seat a person is in and wrong for the review queue's: it was
/// handed its reviewers and holds nothing else in the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskReach {
    /// Any child this session started, and - for a message - any live session's id.
    Anywhere,
    /// Only the children this session's runner holds: the review queue's own reviewers.
    OwnChildren,
}

/// The refusal for a handle a [`TaskReach::OwnChildren`] seat may not address - the handle the
/// caller named, after names and slugs have resolved, so the sentence quotes what it asked with.
fn outside_its_children(
    reach: TaskReach,
    runner: &dyn TaskRunner,
    handle: &str,
    verb: &str,
) -> Option<Invocation> {
    if reach == TaskReach::Anywhere || runner.children().iter().any(|c| c.handle == handle) {
        return None;
    }
    let did = if verb == "task_message" {
        "sent"
    } else {
        "read"
    };
    Some(Invocation::failed(
        format!("`{handle}` is not one of this session's children, so `{verb}` cannot address it"),
        format!(
            "this seat reads and corrects the children it started - here the reviewers the queue's \
             clock started for the rows on its board - and no other session in the daemon. The \
             handles it holds are the ones in `{verb}`'s listing. Nothing was {did}."
        ),
    ))
}

/// `task_result` - collect a subagent `task` started.
pub struct TaskResultTool {
    runner: Arc<dyn TaskRunner>,
    reach: TaskReach,
}

impl TaskResultTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskResultTool {
            runner,
            reach: TaskReach::Anywhere,
        }
    }

    /// Narrow what this seat may read to the children it started - see [`TaskReach`].
    pub fn with_reach(mut self, reach: TaskReach) -> Self {
        self.reach = reach;
        self
    }
}

impl Tool for TaskResultTool {
    fn schema(&self) -> ToolSchema {
        // A seat handed only its own children is told so: a description naming what the seat will
        // refuse is the prompt contradicting the tool, which is what this pair was seated to end.
        let only_its_own = match self.reach {
            TaskReach::Anywhere => "",
            TaskReach::OwnChildren => {
                " Only the children this session started may be read: it holds the reviewers it was \
                 handed and no other session, and a handle that is not one is refused by name."
            }
        };
        ToolSchema::new(
            "task_result",
            format!(
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
             session started.{only_its_own}"
            ),
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
                // The listing says the state, not liveness: `running` about a child between turns
                // is the sentence this stops printing, and the word is the one the single-handle
                // answer uses. A child with no record is marked as one and says how far it got.
                let state = match (&c.lost, &status) {
                    // Answered, and it is NOT a finished answer - nothing recorded a
                    // settlement, so the text is whatever the child had written when it stopped.
                    (Some(LostChild::Answered), _) => format!(
                        "answered, read from its own session — {NOT_A_FINISHED_ANSWER}; no record \
                         of it was ever written"
                    ),
                    (Some(LostChild::Stopped(where_)), _) => format!(
                        "gone with no answer — it stopped {where_}; nothing is running it, and no \
                         record of it was ever written"
                    ),
                    (None, TaskStatus::Running { note, .. }) => match (status.doing(), note) {
                        (Some(doing), Some(n)) => format!("{} — {n}", doing.said()),
                        (Some(doing), None) => doing.said().to_string(),
                        (None, Some(n)) => format!("running — {n}"),
                        (None, None) => "running".into(),
                    },
                    (None, TaskStatus::Done { .. }) => "answered".into(),
                    (None, TaskStatus::Failed { .. }) => "failed".into(),
                    (None, TaskStatus::Unknown) => "unknown".into(),
                };
                out.push_str(&format!("  {} — {state}\n", c.label()));
            }
            return Invocation::ok(out);
        };
        // The handle, or the child's name - the job rule: the id wins, a name two children
        // carry is refused naming them, and anything else goes on as it was written.
        let handle = match resolve_child(self.runner.as_ref(), handle) {
            Ok(h) => h,
            Err(refused) => return refused,
        };
        let handle = handle.as_str();
        if let Some(refused) =
            outside_its_children(self.reach, self.runner.as_ref(), handle, "task_result")
        {
            return refused;
        }
        // Asked BY HANDLE, not looked up in the list. The list carries only the children a
        // record was due for; a child from before the store recorded any is one a model may still
        // name, and its answer needs the same caveat.
        let lost = self.runner.lost(handle);
        let label = self.runner.label_of(handle);
        // A deadline always applies, and its expiry is its own outcome rather
        // than being reported as completion - `job_wait`'s rule, for the same
        // reason: a wait that ran out and a child that answered are two facts.
        let timeout = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(std::time::Duration::from_millis)
            .unwrap_or(std::time::Duration::ZERO);
        // A watched child is looked at, not waited on: the daemon hands its answer to the model as
        // a turn of its own, so a wait would hold the floor while the answer is already on its way.
        // Unwatched handles keep the blocking wait they asked for.
        let timeout = if !timeout.is_zero() && ctx.completion_delivered(handle) {
            // And the wait is the model saying it wants this one: the answer is
            // delivered the moment it lands rather than held for the next break, which is the
            // promise the sentence below makes.
            ctx.want_completion(handle, crate::runtime::WantedNow::Awaited);
            let status = self.runner.look(handle, std::time::Duration::ZERO);
            match &status {
                TaskStatus::Running { note, .. } => {
                    // The steering clause, in a value rather than inside the `format!`: the
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
                        // The spawn's own line, and it is named as the spawn's: the note is written
                        // by the progress callback during the child's opening phase and never
                        // again, so it is not the child's last word. That is in the reading above.
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
        // An answer or a failure read here has been handed to the model - the delivery the
        // watcher would otherwise make - so the runner marks it read, and a daemon restart does
        // not announce it again as news.
        if matches!(status, TaskStatus::Done { .. } | TaskStatus::Failed { .. }) {
            self.runner.consumed(handle);
        }
        match status {
            TaskStatus::Done { ref answer, .. } => {
                // A child with no record's text is not a finished answer, and it is said BEFORE the
                // text: nothing recorded its settlement, so the only witness is its own last row,
                // and an interrupted turn comes back as an answer with whatever it wrote.
                let answer = match &lost {
                    Some(LostChild::Answered) => format!(
                        "{NOT_A_FINISHED_ANSWER}: nothing recorded this child's settlement, so \
                         nothing says its turn ended. `{handle}` and its transcript hold everything \
                         it did — `transcript` with session=\"{handle}\" reads it.\n\n{answer}"
                    ),
                    _ => answer.clone(),
                };
                // The child has finished, so its branch goes to be landed: the one door the tool
                // layer has onto a child's finish. A plain `task` child answers `NoBranch`, which
                // is why nothing is appended for it. The answer comes first, the queue line after.
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
                    // A failure is said, not swallowed. The branch is not pushed and nobody
                    // else carries it, so a parent told nothing would believe the work was on
                    // its way to main when it is sitting on a branch in a worktree.
                    Err(why) => Invocation::ok(format!(
                        "{answer}\n\n[merge queue] the branch of `{label}` was NOT enqueued: \
                         {why} Its worktree and branch are untouched — nothing has been deleted \
                         — but nothing will land it until it is enqueued.{}",
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
            // The sentence that said one thing about three states: "still working" was printed
            // about a child working, one parked after its turn and one wedged mid-call. The
            // headline now names the state, and a look is not a wait that ran out.
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

/// The state of a child whose turn has not ended, in words: `working`, `between turns`, `quiet` or
/// `showing nothing yet` when the daemon has a reading, `still working` when it has none. One
/// function, because the no-wait and deadline sentences must not disagree about which state it is.
fn doing_word(status: &TaskStatus) -> String {
    match status.doing() {
        Some(doing) => doing.said().to_string(),
        None => "still working".to_string(),
    }
}

/// `task_result`'s half of the child's state, with the name attached - the state word and the
/// child's handle, for a sentence that has to say which child it is about.
fn running_said(label: &str, status: &TaskStatus) -> String {
    format!("`{label}` is {}", doing_word(status))
}

/// The one handle a session may address its parent by: `parent`. Reserved, not resolved as a name:
/// it is read before [`resolve_child`], so a session whose id or slug is literally `parent` can
/// never be reached by it. Not an authority: a child's message is recorded as the child's.
pub const PARENT_HANDLE: &str = "parent";

/// The session a `task`-shaped handle names, with [`PARENT_HANDLE`] answered first: `parent` is
/// reserved, so [`TaskRunner::parent`] answers it and nothing else, and only another handle goes on
/// to [`resolve_child`]'s handles-and-slugs resolution.
#[allow(clippy::result_large_err)] // Invocation is the failed row the caller shows; boxing it reshapes the tool API
fn addressed(runner: &dyn TaskRunner, asked: &str) -> Result<String, Invocation> {
    if asked.trim() != PARENT_HANDLE {
        return resolve_child(runner, asked);
    }
    match runner.parent() {
        Some(id) => Ok(id),
        None => Err(Invocation::failed(
            "this session has no parent to speak to",
            format!(
                "`{PARENT_HANDLE}` is the session that started this one, and nothing started \
                 this one: a session at the top of its tree is addressed by its own id. Nothing \
                 was sent."
            ),
        )),
    }
}

/// `task_message`: say something to a session, running or not. The third thing a parent does to
/// work it handed off: `task` starts a child, `task_result` reads it, `job_kill` stops it, and this
/// steers it. The handle may name any live session, child or peer.
pub struct TaskMessageTool {
    runner: Arc<dyn TaskRunner>,
    reach: TaskReach,
}

impl TaskMessageTool {
    pub fn new(runner: Arc<dyn TaskRunner>) -> Self {
        TaskMessageTool {
            runner,
            reach: TaskReach::Anywhere,
        }
    }

    /// Narrow who this seat may address to the children it started - see [`TaskReach`].
    pub fn with_reach(mut self, reach: TaskReach) -> Self {
        self.reach = reach;
        self
    }
}

impl Tool for TaskMessageTool {
    fn schema(&self) -> ToolSchema {
        // Same rule as `task_result`'s: the seat that may address only its own children says so in
        // its own description, instead of promising a door onto every session and refusing it.
        let (who, only_its_own) = match self.reach {
            TaskReach::Anywhere => ("the id of any live session", ""),
            TaskReach::OwnChildren => (
                "the handle of one of this session's children",
                " Only the children this session started may be addressed: it holds the reviewers \
                 it was handed and no other session, and any other handle is refused by name.",
            ),
        };
        ToolSchema::new(
            "task_message",
            format!(
                "Say something to a session — a live correction. Give \
                 `task` (the handle `task` returned, {who}, or `parent` for the \
                 session that started this one) and `text` (what to say). It is not a second \
                 prompt: the session hears it as your message and carries on. A session whose turn \
                 is running hears it at its next round boundary; a session that has answered and is \
                 waiting is woken and runs a turn for it. Refused by name if no session of that id \
                 is live. Read a subagent with `task_result`; stop one with `job_kill`.{only_its_own}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": format!("The handle `task` returned (or the child's slug), {who} to address, or `parent` — the session that started this one.")},
                    "text": {"type": "string", "description": "What to say to the session. It keeps working; this steers what it does next."}
                },
                "required": ["task", "text"]
            }),
            // `Session`, exactly as `task` and `task_result` are: this reaches a child's own
            // session and nothing on the host. The child's own gate governs whatever it does
            // with what it is told - a parent cannot use a message to launder a capability.
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
        let handle = match addressed(self.runner.as_ref(), handle) {
            Ok(h) => h,
            Err(refused) => return refused,
        };
        let handle = handle.as_str();
        if let Some(refused) =
            outside_its_children(self.reach, self.runner.as_ref(), handle, "task_message")
        {
            return refused;
        }
        match self.runner.send(handle, text) {
            Ok(said) => Invocation::ok(said),
            // A refusal comes back unchanged, like `job_kill`'s: the runner is the only thing
            // that knows whether the child heard it, and a tool that reformatted its refusal
            // into a success is the defect this tree already paid for once.
            Err(why) => Invocation::failed(format!("subagent `{handle}` was not messaged"), why),
        }
    }
}

/// `task_resume`: bring back a subagent a daemon restart left unfinished, in one call. It resumes
/// the child in its own session, same id, transcript, worktree, branch, role and model, and
/// continues its turn with a restart notice.
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
            // `Backgrounded`, as `task`'s is - the child is working again and has not
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
