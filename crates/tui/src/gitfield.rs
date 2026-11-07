//! What the workspace's own repository says, for the row the operator crosses: the facts and not
//! the text, because the format decides the text and a preference may change it. A repository this
//! cannot read is ABSENT, not empty, and the read is STARTED from the loop rather than waited on.

use std::process::Command;

/// How many columns a segment's text takes - the same ANSI-aware measure the header centres with.
/// A piece's width is what fitting spends and the pieces are plain text anyway, so this stays
/// local rather than pulling a renderer import in for one `usize`.
fn visible_width(s: &str) -> usize {
    rano::width::text::width(s)
}

/// How long a reading is trusted before the loop takes another.
pub const GIT_REFRESH_MS: u64 = 2_000;

/// How long one reading may take before it is killed and treated as absent.
pub const GIT_TIMEOUT_SECS: u64 = 5;

/// The default format: gitstatus's segments, in gitstatus's order. The placeholders ARE the
/// glyphs' meaning - `%b` branch, `%d` behind, `%a` ahead, `%s` stashes, `%m` the action in
/// progress, `%~` conflicts, `%+` staged, `%!` unstaged, `%?` untracked; `git_format` overrides it.
pub const GIT_FORMAT_DEFAULT: &str = "%b%d%a%s%m%~%+%!%?";

/// One reading of the repository, as FACTS and not as text. The counts are `Some` only when there
/// is something to say - a segment at zero is one the format does not draw - and `branch` already
/// carries the detached spelling, because two consumers would otherwise spell it two ways.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    /// The branch name, or `@oid` (eight) when detached, or `@` alone when detached with no oid.
    pub branch: String,
    pub detached: bool,
    pub behind: Option<usize>,
    pub ahead: Option<usize>,
    pub stash: Option<usize>,
    /// rebase / merge / cherry-pick / revert / bisect - from the marker files, not from
    /// `git status`, which does not report one in porcelain at all.
    pub action: Option<&'static str>,
    pub conflict: Option<usize>,
    pub staged: Option<usize>,
    pub unstaged: Option<usize>,
    pub untracked: Option<usize>,
}

/// The style a segment is painted with: one role per segment, chosen to say what the segment SAYS
/// rather than to be pretty. A branch is green when the tree is clean and yellow when it is not,
/// and conflicts are red because nothing else on that row is a demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRole {
    BranchClean,
    BranchDirty,
    Behind,
    Ahead,
    Stash,
    Action,
    Conflict,
    Staged,
    Unstaged,
    Untracked,
}

/// `git status --porcelain=v2 --branch --show-stash` as the facts - leticl's `%git-parts`. The v2 shape this reads:
///
/// ```text
/// # branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce
/// # branch.head main
/// # branch.upstream origin/main
/// # branch.ab +0 -0
/// # stash 2
/// 1 .M N... 100644 100644 100644 aaa bbb src/app.rs
/// u N... 100644 100644 100644 aaa bbb src/conflict.rs
/// ? src/untracked.rs
/// ```
///
/// Every `1 XY`/`2 XY` line is one changed path: X is index-vs-HEAD and Y is workdir-vs-index, so `.` in X with `M` in Y is unstaged
/// work and the reverse is staged (`%+` against `%!`). `u` lines are unmerged, `?` untracked. `branch.ab` and `branch.upstream` are
/// absent on a branch with no upstream, which is not an error. `# stash N` needs `--show-stash`.
///
/// `None` when the text names no branch at all: an empty reading or git's own complaint means *not a repository I can read*.
pub fn git_parts(porcelain: &str, action: Option<&'static str>) -> Option<GitState> {
    let mut head: Option<String> = None;
    let mut oid: Option<String> = None;
    let mut ab: Option<(u64, u64)> = None;
    let mut stash: Option<usize> = None;
    let mut staged = 0usize;
    let mut unstaged = 0usize;
    let mut untracked = 0usize;
    let mut conflict = 0usize;
    for line in porcelain.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            head = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.oid ") {
            oid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut it = rest.split_whitespace();
            let ahead = it
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|n| n.parse::<u64>().ok());
            let behind = it
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|n| n.parse::<u64>().ok());
            if let (Some(a), Some(b)) = (ahead, behind) {
                ab = Some((a, b));
            }
        } else if let Some(rest) = line.strip_prefix("# stash ") {
            stash = rest.trim().parse::<usize>().ok();
        } else if let Some("1" | "2") = line.get(0..1) {
            // X is index-vs-HEAD, Y is workdir-vs-index, and a `.` means that side is clean. The
            // guard is the prefix's own length: `line.get(3..4)` on a shorter line is `None`, not
            // `false`.
            if line.get(2..3).is_some_and(|x| x != ".") {
                staged += 1;
            }
            if line.get(3..4).is_some_and(|y| y != ".") {
                unstaged += 1;
            }
        } else if line.starts_with('u') {
            conflict += 1;
        } else if line.starts_with('?') {
            untracked += 1;
        }
    }
    let head = head?;
    let (branch, detached) = if head == "(detached)" {
        // A detached HEAD is not branch-less. gitstatus shows the commit and not the
        // branch, `@` plus the first eight of the oid - the width its own README prints.
        let at = oid
            .as_deref()
            .map(|o| o.chars().take(8).collect::<String>());
        (format!("@{}", at.unwrap_or_default()), true)
    } else {
        (head, false)
    };
    let pos = |n: u64| (n > 0).then_some(n as usize);
    Some(GitState {
        branch,
        detached,
        behind: ab.and_then(|(_, b)| pos(b)),
        ahead: ab.and_then(|(a, _)| pos(a)),
        stash: stash.filter(|n| *n > 0),
        action,
        conflict: (conflict > 0).then_some(conflict),
        staged: (staged > 0).then_some(staged),
        unstaged: (unstaged > 0).then_some(unstaged),
        untracked: (untracked > 0).then_some(untracked),
    })
}

/// One segment's `(text, role)` - leticl's `%git-segment`. Glyphs are gitstatus's own.
fn git_segment(state: &GitState, slot: GitSlot) -> Option<(String, GitRole)> {
    let dirty = state.staged.is_some()
        || state.unstaged.is_some()
        || state.untracked.is_some()
        || state.conflict.is_some();
    match slot {
        GitSlot::Branch => Some((
            state.branch.clone(),
            if dirty {
                GitRole::BranchDirty
            } else {
                GitRole::BranchClean
            },
        )),
        GitSlot::Action => state.action.map(|a| (a.to_string(), GitRole::Action)),
        GitSlot::Behind => state.behind.map(|n| (format!("⇣{n}"), GitRole::Behind)),
        GitSlot::Ahead => state.ahead.map(|n| (format!("⇡{n}"), GitRole::Ahead)),
        GitSlot::Stash => state.stash.map(|n| (format!("*{n}"), GitRole::Stash)),
        GitSlot::Conflict => state.conflict.map(|n| (format!("~{n}"), GitRole::Conflict)),
        GitSlot::Staged => state.staged.map(|n| (format!("+{n}"), GitRole::Staged)),
        GitSlot::Unstaged => state.unstaged.map(|n| (format!("!{n}"), GitRole::Unstaged)),
        GitSlot::Untracked => state
            .untracked
            .map(|n| (format!("?{n}"), GitRole::Untracked)),
    }
}

/// The format's placeholders - leticl's `+git-slots+`, in the order the default spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitSlot {
    Branch,
    Behind,
    Ahead,
    Stash,
    Action,
    Conflict,
    Staged,
    Unstaged,
    Untracked,
}

impl GitSlot {
    /// `%b` and friends. An enum rather than the string, so a template's `%x` cannot be read as
    /// a slot this build does not have.
    fn of(two: &str) -> Option<GitSlot> {
        Some(match two {
            "%b" => GitSlot::Branch,
            "%d" => GitSlot::Behind,
            "%a" => GitSlot::Ahead,
            "%s" => GitSlot::Stash,
            "%m" => GitSlot::Action,
            "%~" => GitSlot::Conflict,
            "%+" => GitSlot::Staged,
            "%!" => GitSlot::Unstaged,
            "%?" => GitSlot::Untracked,
            _ => return None,
        })
    }
}

/// STATE as the pieces the FORMAT asks for. Literals attach to the piece they PRECEDE, so fitting
/// drops whole pieces and never half of one; a literal before a segment with nothing to say is
/// dropped with it, since a separator whose mark is absent is a dangling space.
pub fn git_pieces(state: &GitState, format: &str) -> Vec<(String, GitRole)> {
    let chars: Vec<char> = format.chars().collect();
    let mut out: Vec<(String, GitRole)> = Vec::new();
    let mut pending = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '%' {
            pending.push(chars[i]);
            i += 1;
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if let Some(slot) = GitSlot::of(&two) {
            if let Some((text, role)) = git_segment(state, slot) {
                pending.push_str(&text);
                out.push((std::mem::take(&mut pending), role));
            } else {
                // The segment has nothing to say: its literal goes with it.
                pending.clear();
            }
            i += 2;
        } else {
            // `%%` is a literal per cent, and a trailing `%` is literal too.
            pending.push('%');
            i += if two.len() == 2 { 2 } else { 1 };
        }
    }
    if !pending.is_empty()
        && let Some(last) = out.last_mut()
    {
        last.0.push_str(&pending);
    }
    out
}

/// The longest PREFIX of the pieces that fits in ROOM columns. The field degrades by DELETION: the
/// branch is the floor and the marks fall off its right in gitstatus's order, so a narrow screen
/// loses `?4` and not the branch.
pub fn git_fit(pieces: &[(String, GitRole)], room: usize) -> Vec<&(String, GitRole)> {
    let mut out = Vec::new();
    let mut used = 3usize; // the " (" and ")" the row wraps the field in
    for piece in pieces {
        let w = visible_width(&piece.0);
        if used + w + 1 > room {
            break;
        }
        used += w;
        out.push(piece);
    }
    out
}

/// An action in progress, from the marker files git itself leaves: porcelain reports none at all,
/// so the markers are the only source. A `.git` that is a FILE is a linked worktree whose markers
/// live beside the real repository, and this answers `None` for it - an absence, not a guess.
pub fn git_action(workspace: &str) -> Option<&'static str> {
    let git = std::path::Path::new(workspace).join(".git");
    if !git.is_dir() {
        return None;
    }
    let there = |n: &str| git.join(n).exists();
    if there("rebase-merge") || there("rebase-apply") {
        Some("rebase")
    } else if there("MERGE_HEAD") {
        Some("merge")
    } else if there("CHERRY_PICK_HEAD") {
        Some("cherry-pick")
    } else if there("REVERT_HEAD") {
        Some("revert")
    } else if there("BISECT_LOG") {
        Some("bisect")
    } else {
        None
    }
}

/// One directory down, when the repository is not the workspace: the workspace when it is a
/// repository, else the same-named child, else the single repository child, else the workspace
/// unchanged. A `TODO.md` is not evidence that it is the project; [`todos_path`] asks that.
pub fn project_dir(ws: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(ws);
    let is_repo = |d: &std::path::Path| d.join(".git").exists();
    if is_repo(dir) || dir.read_dir().is_err() {
        return dir.to_path_buf();
    }
    // The same-named child first: nested naming is a convention, not a coincidence.
    let own_name = dir.file_name().map(std::ffi::OsStr::to_owned);
    if let Some(name) = own_name
        && is_repo(&dir.join(&name))
    {
        return dir.join(name);
    }
    // Then a single repository child - and only one. Two is a choice this function does not make.
    let repos: Vec<std::path::PathBuf> = dir
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| is_repo(p))
        .collect();
    match repos.as_slice() {
        [only] => only.clone(),
        _ => dir.to_path_buf(),
    }
}

/// Which `TODO.md` the operator's queue is: the workspace's own when it has one, else the
/// project's. A separate question from [`project_dir`], because a directory carrying a list says
/// nothing about where the branch is, and a session in a worktree has the worktree's own list.
pub fn todos_path(ws: &str) -> std::path::PathBuf {
    let own = std::path::Path::new(ws).join("TODO.md");
    if own.is_file() {
        return own;
    }
    project_dir(ws).join("TODO.md")
}

/// The repository the project is: [`project_dir`]'s answer when that is one or sits inside one
/// (git walks up), else the one repository child of a project folder that is not itself a
/// repository. Separate from [`project_dir`] because a folder may carry the list and not the code.
pub fn repo_dir(ws: &str) -> std::path::PathBuf {
    let project = project_dir(ws);
    repo_of(&project.display().to_string()).unwrap_or(project)
}

/// The repository `dir` is or sits inside, or the one repository it holds. `None` for a folder
/// holding several: the header's queue door scopes by path prefix, so a folder would open an empty
/// pane, and no door is better than a wrong one.
pub fn repo_of(dir: &str) -> Option<std::path::PathBuf> {
    if dir.is_empty() {
        return None;
    }
    let d = std::path::Path::new(dir);
    let is_repo = |p: &std::path::Path| p.join(".git").exists();
    // Git walks up from `d` on its own for a workspace inside a repository.
    if d.ancestors().any(is_repo) {
        return Some(d.to_path_buf());
    }
    let repos: Vec<std::path::PathBuf> = d
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| is_repo(p))
        .collect();
    match repos.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// One reading of the workspace, or `None`. The argv carries `--no-optional-locks` because a plain
/// `git status` may write the index back - a reader writing to the repository it reports on - and
/// `--show-stash` because that is where gitstatus's `%s` comes from.
pub fn read_state(workspace: &str) -> Option<GitState> {
    if workspace.is_empty() {
        return None;
    }
    // Resolved, not assumed - the workspace may be the parent of the repository rather than
    // the repository (`repo_dir`), which is the nested layout both this field and the todos
    // pane were drawing nothing over.
    read_state_at(&repo_dir(workspace))
}

/// The repositories a workspace holds: one entry when the workspace is a repository or sits inside
/// one (git walks up), otherwise every child that is one, the same-named child first and the rest
/// by name. Empty for a folder that holds none.
pub fn repos_in(ws: &str) -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(ws);
    if ws.is_empty() {
        return Vec::new();
    }
    if dir.ancestors().any(|a| a.join(".git").exists()) {
        return vec![dir.to_path_buf()];
    }
    let own = dir.file_name().map(std::ffi::OsStr::to_owned);
    let mut repos: Vec<std::path::PathBuf> = dir
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    repos.sort_by_key(|p| (p.file_name() != own.as_deref(), p.clone()));
    repos
}

/// The repository of `path` among a workspace's - `path` relative to the workspace or
/// absolute: the repository that holds it, when it is one of [`repos_in`]'s.
pub fn repo_holding(ws: &str, path: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::path::Path::new(ws).join(p)
    };
    repos_in(ws)
        .into_iter()
        .filter(|r| full.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
}

/// One reading of the repository at `dir`, or `None` - see [`read_state`].
pub fn read_state_at(dir: &std::path::Path) -> Option<GitState> {
    let mut git = Command::new("git");
    git.arg("-C")
        .arg(dir)
        // A reader writes nothing to the repository it reports on.
        .arg("--no-optional-locks")
        .args(["status", "--porcelain=v2", "--branch", "--show-stash"]);
    let stdout = run_with_deadline(git, std::time::Duration::from_secs(GIT_TIMEOUT_SECS))?;
    let text = String::from_utf8_lossy(&stdout);
    let action = git_action(dir.to_str()?);
    git_parts(&text, action)
}

/// A command's stdout if it succeeds within `limit`, else `None` - and a command past its limit is
/// killed, not waited for. The output is drained on a thread so a reading larger than the pipe
/// cannot stall the child while the deadline is watched.
pub fn run_with_deadline(cmd: Command, limit: std::time::Duration) -> Option<Vec<u8>> {
    match run_until(cmd, limit) {
        Ran::Ok(out) => Some(out),
        _ => None,
    }
}

/// How a command run under a deadline ended.
#[derive(Debug)]
pub enum Ran {
    /// It exited 0, with this stdout.
    Ok(Vec<u8>),
    /// It exited non-zero.
    Failed,
    /// It could not be started.
    NotStarted(std::io::Error),
    /// It was still running at the deadline and was killed.
    TimedOut,
}

/// [`run_with_deadline`], saying which way it ended.
pub fn run_until(mut cmd: Command, limit: std::time::Duration) -> Ran {
    use std::io::Read;
    let mut child = match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Ran::NotStarted(e),
    };
    let Some(mut pipe) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Ran::Failed;
    };
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    });
    let until = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => {
                // Killed, and the reader is NOT joined: what git started (a submodule's own
                // status) can hold the pipe open past git, and joining would wait for it - the
                // very wait the deadline exists to bound. The thread ends when the pipe does.
                let _ = child.kill();
                let _ = child.wait();
                return Ran::TimedOut;
            }
        }
    };
    match reader.join() {
        Ok(out) if status.success() => Ran::Ok(out),
        _ => Ran::Failed,
    }
}

/// [`run_until`], keeping stderr and the exit status, for a caller that has to tell a refusal from
/// an empty answer. `Err(None)` is the deadline passing, and the program was killed.
pub fn output_until(
    mut cmd: Command,
    limit: std::time::Duration,
) -> Result<std::process::Output, Option<std::io::Error>> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(Some)?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let until = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            // Killed, and the readers are not joined, for the reason `run_until` gives.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(None);
            }
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod deadline_tests {
    use super::run_with_deadline;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn output_until_keeps_stderr_and_status_and_kills_at_the_deadline() {
        let out = super::output_until(
            sh("printf out; printf err >&2; exit 3"),
            Duration::from_secs(5),
        )
        .expect("it ran");
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.status.code(), Some(3));
        let started = Instant::now();
        let late = super::output_until(sh("sleep 30"), Duration::from_millis(200));
        assert!(matches!(late, Err(None)), "{late:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_command_within_its_limit_gives_its_output() {
        let out = run_with_deadline(sh("printf hello"), Duration::from_secs(5));
        assert_eq!(out.as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn a_failing_command_gives_nothing() {
        assert_eq!(
            run_with_deadline(sh("echo no; exit 3"), Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn a_command_past_its_limit_is_killed_rather_than_waited_for() {
        let t = Instant::now();
        assert_eq!(
            run_with_deadline(sh("sleep 30"), Duration::from_millis(200)),
            None
        );
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    /// A grandchild holding the pipe past the killed child does not hold the caller.
    #[test]
    fn a_grandchild_keeping_the_pipe_does_not_outlast_the_limit() {
        let t = Instant::now();
        assert_eq!(
            run_with_deadline(sh("sleep 30 & sleep 30"), Duration::from_millis(200)),
            None
        );
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    /// Output larger than a pipe's buffer is drained while the deadline is watched.
    #[test]
    fn output_larger_than_the_pipe_is_read_whole() {
        let out = run_with_deadline(
            sh("i=0; while [ $i -lt 20000 ]; do echo 0123456789; i=$((i+1)); done"),
            Duration::from_secs(20),
        )
        .expect("it finished");
        assert_eq!(out.len(), 20000 * 11);
    }
}

/// The path a tool call is about, from its arguments - `path`, `file_path` or `file` (what
/// `display_target` reads first), else a `cwd`/`workdir`, else the directory a shell command
/// starts with `cd` into. What the git field follows in a workspace of several repositories.
pub fn call_path(arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    for k in ["path", "file_path", "file", "cwd", "workdir"] {
        if let Some(p) = v
            .get(k)
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty() && *p != ".")
        {
            return Some(p.to_string());
        }
    }
    let cmd = v.get("command").and_then(|c| c.as_str())?.trim_start();
    let rest = cmd.strip_prefix("cd ")?.trim_start();
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '&' || c == ';' || c == '|')
        .unwrap_or(rest.len());
    let dir = rest[..end].trim_matches(['"', '\'']);
    (!dir.is_empty()).then(|| dir.to_string())
}

/// The field as one plain string through the DEFAULT format - what the tests assert, and the
/// shape the first cut of this field drew. No colours, no fitting: the text and nothing else.
pub fn git_text(state: &GitState) -> String {
    git_pieces(state, GIT_FORMAT_DEFAULT)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The README's own example state, one fixture every reader of a segment shares: the
    /// gitstatus table itself - one of everything, so a missing glyph is a wrong line, not a
    /// quiet row.
    fn everything() -> GitState {
        GitState {
            branch: "master".into(),
            detached: false,
            behind: Some(1),
            ahead: Some(2),
            stash: Some(5),
            action: Some("merge"),
            conflict: Some(6),
            staged: Some(7),
            unstaged: Some(8),
            untracked: Some(9),
        }
    }

    #[test]
    fn a_clean_branch_is_its_name() {
        let clean = "# branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce\n# branch.head main\n\
                     # branch.upstream origin/main\n# branch.ab +0 -0\n";
        let state = git_parts(clean, None).unwrap();
        assert_eq!(git_text(&state), "main");
        // A zero ahead/behind is NOTHING to say, not `⇣0⇡0` on every clean tree.
        assert_eq!(state.behind, None);
        assert_eq!(state.ahead, None);
    }

    /// Three facts, three marks: a field that showed `main` for a tree with nine changed files
    /// would be the row lying politely, so the port keeps gitstatus's own separation - X is staged,
    /// Y is unstaged.
    #[test]
    fn a_dirty_branch_separates_staged_from_unstaged() {
        let dirty = "# branch.oid abc1234\n# branch.head main\n# branch.upstream origin/main\n\
                     # branch.ab +2 -1\n# stash 3\n1 .M N... 100644 100644 100644 aaa bbb src/app.rs\n\
                     1 M. N... 100644 100644 100644 ccc ddd src/lib.rs\n\
                     1 MM N... 100644 100644 100644 eee fff src/both.rs\n\
                     ? src/untracked.rs\nu N... 100644 100644 100644 ggg hhh src/conflict.rs\n";
        let state = git_parts(dirty, None).unwrap();
        assert_eq!(
            git_text(&state),
            "main⇣1⇡2*3~1+2!2?1",
            "gitstatus's segments, in gitstatus's order: behind, ahead, stash, conflicts,\
             staged, unstaged, untracked"
        );
        // A branch with no upstream at all: no `branch.ab`, no `branch.upstream`, still a branch.
        let lonely = "# branch.oid abc1234\n# branch.head feature/thing\n";
        assert_eq!(
            git_text(git_parts(lonely, None).as_ref().unwrap()),
            "feature/thing"
        );
    }

    /// A detached HEAD says which commit, because `(detached)` alone is a name with no content
    /// - `@` plus the first eight of the oid, the width gitstatus itself prints.
    #[test]
    fn a_detached_head_is_the_commit_at_gitstatuses_width() {
        let detached = "# branch.oid 31f30adfa3266f3ee47186ae4a33f22a43af50ce\n\
                        # branch.head (detached)\n";
        let state = git_parts(detached, None).unwrap();
        assert!(state.detached);
        assert_eq!(state.branch, "@31f30adf");
    }

    /// An action in progress is a segment, from the marker files - `%m`, magenta and bold,
    /// between the stash and the conflicts: gitstatus's own order.
    #[test]
    fn an_action_is_a_segment_between_the_stash_and_the_conflicts() {
        let dirty = "# branch.oid abc1234\n# branch.head main\n# stash 1\n1 .M N... a b c\n";
        let state = git_parts(dirty, Some("rebase")).unwrap();
        assert_eq!(git_text(&state), "main*1rebase!1");
    }

    /// The template decides the text - the port's own point. Segments the template does not
    /// name are not drawn; literals travel with the piece they precede; `%%` is a per cent; an
    /// unknown `%x` is a literal per cent; a literal before an ABSENT segment is dropped with it.
    #[test]
    fn the_format_decides_the_text() {
        let state = everything();
        // A spaced format: the space travels with the mark that follows it. leticl's preset
        // verbatim - `%b %!%+` - whose space sits before `%!` only, so `+7` follows unspaced.
        let pieces = git_pieces(&state, "%b %!%+");
        assert_eq!(
            pieces.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(),
            vec!["master", " !8", "+7"]
        );
        // The branch alone.
        assert_eq!(
            git_pieces(&state, "%b")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["master"]
        );
        // A literal before an absent segment is dropped with it, and one AFTER it travels with the
        // next segment: literals attach to the piece they precede, so the " (" before an absent
        // stash goes and the ") " that follows attaches to the conflict mark.
        let no_stash = GitState {
            stash: None,
            ..everything()
        };
        assert_eq!(
            git_pieces(&no_stash, "%b (%s) %~")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["master", ") ~6"],
            "the literal before the absent stash went with it; the one after did not"
        );
        // `%%` and an unknown slot - both are a literal per cent, and the unknown slot's
        // letter is CONSUMED with it (leticl skips two, the `%` and the letter).
        assert_eq!(
            git_pieces(&state, "%% %z%b")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["% %master"]
        );
        // A trailing literal attaches to the last piece - the same rule, read from the other end.
        assert_eq!(
            git_pieces(&state, "%b!")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>(),
            vec!["master!"]
        );
    }

    /// The branch is the floor - fitting drops whole pieces from the right, so a narrow
    /// screen loses `?9` and not the branch.
    #[test]
    fn fitting_drops_whole_pieces_and_keeps_the_branch() {
        let pieces = git_pieces(&everything(), GIT_FORMAT_DEFAULT);
        let full: Vec<&str> = git_fit(&pieces, 80).iter().map(|p| p.0.as_str()).collect();
        assert_eq!(
            full,
            vec!["master", "⇣1", "⇡2", "*5", "merge", "~6", "+7", "!8", "?9"]
        );
        // Room for the branch and the first two marks only.
        let tight: Vec<&str> = git_fit(&pieces, 14).iter().map(|p| p.0.as_str()).collect();
        assert_eq!(tight, vec!["master", "⇣1", "⇡2"]);
        // Not even room for the branch: nothing, not a truncated half of one.
        assert!(git_fit(&pieces, 8).is_empty());
    }

    /// The branch's colour is the one fact a person reads at a glance - clean green, dirty
    /// yellow - and it is dirty on ANY of the four work counts, untracked included.
    #[test]
    fn the_branch_role_says_whether_the_tree_is_clean() {
        let clean = git_parts(
            "# branch.oid abc1234\n# branch.head main\n# branch.ab +0 -0\n",
            None,
        )
        .unwrap();
        assert_eq!(
            git_pieces(&clean, "%b")[0].1,
            GitRole::BranchClean,
            "a clean tree is green"
        );
        for mutate in [
            |s: &mut GitState| s.staged = Some(1),
            |s: &mut GitState| s.unstaged = Some(1),
            |s: &mut GitState| s.untracked = Some(1),
            |s: &mut GitState| s.conflict = Some(1),
        ] {
            let mut dirty = clean.clone();
            mutate(&mut dirty);
            assert_eq!(
                git_pieces(&dirty, "%b")[0].1,
                GitRole::BranchDirty,
                "any work in the tree yellows the branch"
            );
        }
    }
}
