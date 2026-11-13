//! Work a VM's copy may hold that exists nowhere else: what is checked before the booted copy is
//! removed, and what is carried out of the guest before it goes down (`docs/lifecycle.md` d.8).

use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The guest's commits and uncommitted work, saved on the host as a bundle before the VM stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rescue {
    pub bundle: PathBuf,
    /// The guest's HEAD.
    pub head: String,
    /// Commits past the base, when the base was known.
    pub commits: Option<u64>,
    pub dirty: bool,
}

impl Rescue {
    /// The ref inside the bundle: HEAD, or a commit of the dirty tree on top of it.
    pub const REF: &'static str = "refs/letibot/rescue";

    pub fn what(&self) -> String {
        let commits = match self.commits {
            Some(0) | None => None,
            Some(1) => Some("1 commit".to_string()),
            Some(n) => Some(format!("{n} commits")),
        };
        match (commits, self.dirty) {
            (Some(c), true) => format!("{c} and uncommitted changes"),
            (Some(c), false) => c,
            (None, true) => "uncommitted changes".into(),
            (None, false) => format!("HEAD {}", short(&self.head)),
        }
    }

    pub fn fetch_line(&self) -> String {
        format!(
            "`git fetch {} {}:refs/heads/rescued`",
            self.bundle.display(),
            Self::REF
        )
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn git(dir: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.arg("-C")
        .arg(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    c
}

fn out(mut c: std::process::Command) -> Option<Vec<u8>> {
    let o = c.output().ok()?;
    o.status.success().then_some(o.stdout)
}

/// HEAD of the repository at `dir`, when it is one with a commit.
pub fn git_head(dir: &Path) -> Option<String> {
    let mut c = git(dir);
    c.args(["rev-parse", "-q", "--verify", "HEAD"]);
    let s = String::from_utf8(out(c)?).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Why `copy` must not be removed, or `None` when everything in it is also in `source`: a HEAD
/// no ref of the source contains, or a change whose content the source does not have. A path
/// the copy left out by `exclude` is not a change.
pub fn holds_work(copy: &Path, source: &Path, exclude: &[String]) -> Option<String> {
    let head = git_head(copy)?;
    if git_head(source).is_some() {
        let mut c = git(source);
        c.args([
            "for-each-ref",
            "--count=1",
            "--format=x",
            "--contains",
            &head,
        ]);
        let reachable = out(c).is_some_and(|o| !o.is_empty());
        if !reachable {
            return Some(format!(
                "its HEAD {} is in no ref of {}",
                short(&head),
                source.display()
            ));
        }
    }
    let mut c = git(copy);
    c.args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let Some(status) = out(c) else {
        return Some("`git status` could not be read in it".into());
    };
    let mut changed = Vec::new();
    let mut records = status.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(rec) = records.next() {
        if rec.len() < 4 {
            continue;
        }
        let path = String::from_utf8_lossy(&rec[3..]).into_owned();
        if matches!(rec[0], b'R' | b'C') {
            records.next();
        }
        if !same_in_both(copy, source, &path, exclude) {
            changed.push(path);
        }
    }
    match changed.len() {
        0 => None,
        n => Some(format!(
            "{n} changed path(s) the source does not have, first `{}`",
            changed[0]
        )),
    }
}

fn same_in_both(copy: &Path, source: &Path, path: &str, exclude: &[String]) -> bool {
    let (c, s) = (copy.join(path), source.join(path));
    match (std::fs::symlink_metadata(&c), std::fs::symlink_metadata(&s)) {
        (Err(_), Err(_)) => true,
        (Err(_), Ok(_)) => excluded(path, exclude),
        (Ok(cm), Ok(sm)) if cm.file_type().is_symlink() && sm.file_type().is_symlink() => {
            std::fs::read_link(&c).ok() == std::fs::read_link(&s).ok()
        }
        (Ok(cm), Ok(sm)) if cm.is_file() && sm.is_file() => {
            cm.len() == sm.len() && std::fs::read(&c).ok() == std::fs::read(&s).ok()
        }
        _ => false,
    }
}

fn excluded(path: &str, exclude: &[String]) -> bool {
    let p = format!("/{path}/");
    exclude.iter().any(|e| p.contains(&format!("/{e}/")))
}

/// The guest command that says whether the tree holds work past `base`, and if so stages it as
/// a bundle at [`GUEST_BUNDLE`]. Prints `CLEAN`, or `WORK <head> <commits|-> <dirty 0|1>`.
pub fn guest_stage(project: &str, base: Option<&str>) -> String {
    let base = base.unwrap_or("");
    format!(
        "cd {project} 2>/dev/null || exit 0\n\
         head=$(git rev-parse -q --verify HEAD 2>/dev/null) || exit 0\n\
         base='{base}'\n\
         dirty=0; [ -n \"$(git status --porcelain --untracked-files=normal 2>/dev/null | head -c1)\" ] && dirty=1\n\
         if [ \"$dirty\" = 0 ] && {{ [ -z \"$base\" ] || [ \"$head\" = \"$base\" ]; }}; then echo CLEAN; exit 0; fi\n\
         tip=$head\n\
         if [ \"$dirty\" = 1 ]; then\n\
           idx=$(mktemp); rm -f \"$idx\"\n\
           GIT_INDEX_FILE=$idx git read-tree HEAD || exit 3\n\
           GIT_INDEX_FILE=$idx git add -A || exit 3\n\
           tree=$(GIT_INDEX_FILE=$idx git write-tree) || exit 3\n\
           rm -f \"$idx\"\n\
           tip=$(git -c user.name=letibot -c user.email=letibot@localhost commit-tree -p HEAD \
                 -m 'letibot: uncommitted work when the VM went down' \"$tree\") || exit 3\n\
         fi\n\
         git update-ref {r} \"$tip\" || exit 4\n\
         n=-\n\
         if [ -n \"$base\" ] && git cat-file -e \"$base^{{commit}}\" 2>/dev/null; then\n\
           n=$(git rev-list --count \"$base..$head\")\n\
           [ \"$tip\" = \"$base\" ] || git bundle create {b} {r} \"^$base\" >/dev/null 2>&1 || exit 5\n\
         else\n\
           git bundle create {b} {r} >/dev/null 2>&1 || exit 5\n\
         fi\n\
         echo \"WORK $head $n $dirty\"",
        r = Rescue::REF,
        b = GUEST_BUNDLE,
    )
}

/// Where the guest stages the bundle before it is read out.
pub const GUEST_BUNDLE: &str = "/tmp/letibot-rescue.bundle";

/// A bundle larger than this is not read out over `firecode in`; the refusal is said instead.
pub const MAX_BUNDLE: u64 = 512 * 1024 * 1024;

/// `WORK <head> <commits|-> <dirty>`, parsed.
pub fn parse_stage(line: &str) -> Option<(String, Option<u64>, bool)> {
    let mut w = line.split_whitespace();
    if w.next()? != "WORK" {
        return None;
    }
    let head = w.next()?.to_string();
    let commits = w.next()?.parse().ok();
    let dirty = w.next()? == "1";
    Some((head, commits, dirty))
}
