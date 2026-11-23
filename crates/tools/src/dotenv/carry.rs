//! Carrying a project's `.env` files to a copy of its tree: a package worktree, another machine, a
//! guest. They are untracked, so neither `git worktree add` nor a bundle nor a fetch brings them,
//! and a session placed on a copy would otherwise start without its project's variables.
//!
//! Three rules, and each one is a test: the mode is 0600, the bytes are never said in a sentence
//! (a name is all a line may carry), and a carried file git does not already ignore is added to
//! `.git/info/exclude` so it can never be committed.

use std::path::{Path, PathBuf};

/// The files a copy of the tree gets, as `(name, source path)`, from the directories in order: the
/// first directory holding a name is the one it is read from. The source checkout's root and the
/// package's repository are usually the same directory; two names for one tree is not an error.
pub fn found(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for dir in dirs {
        for name in super::names_in(dir) {
            if !out.iter().any(|(had, _)| *had == name) {
                let at = dir.join(&name);
                out.push((name, at));
            }
        }
    }
    out
}

/// The bytes of one carried file, refused when it is not a plain file: a symlink into a credential
/// store is not a file to copy, and `~/.netrc` linked as `.env` is exactly what that refusal is for.
pub fn read_one(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "{} is not a regular file, so it was not carried",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write one carried file into `to` at mode 0600.
pub fn write_one(to: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let target = to.join(name);
    std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    set_private(&target)
}

/// 0600, because these files hold credentials: a copied file with the source's mode is one other
/// users on that machine could read. A symlink is refused rather than followed - `set_permissions`
/// would chmod whatever it points at, and a link named `.env` at `~/.netrc` is exactly that.
pub fn set_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink, so it was left alone rather than made private",
            path.display()
        ));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Carry `files` into the tree at `to`: copy each at mode 0600, then make git ignore the ones it
/// does not already. Returns the names carried, in the order they were found. A file that cannot
/// be read is said and skipped - one broken file must not cost a placement.
pub fn into_tree(files: &[(String, PathBuf)], to: &Path) -> (Vec<String>, Vec<String>) {
    let mut carried = Vec::new();
    let mut said = Vec::new();
    for (name, from) in files {
        match read_one(from) {
            Ok(bytes) => match write_one(to, name, &bytes) {
                Ok(()) => carried.push(name.clone()),
                Err(why) => said.push(why),
            },
            Err(why) => said.push(why),
        }
    }
    if let Err(why) = exclude_local(to, &carried) {
        said.push(why);
    }
    (carried, said)
}

/// [`into_tree`] from the directories a session's tree came from to the copy at `to`.
pub fn into_worktree(dirs: &[PathBuf], to: &Path) -> (Vec<String>, Vec<String>) {
    into_tree(&found(dirs), to)
}

/// The names a firecode VM's copy keeps or drops: the bytes arrive with the copy, so what is left to
/// do is the mode when it was carried, and removal when it was not. The default is not carried -
/// these files hold credentials, and a guest is another machine.
pub fn settle_vm_copy(project: &Path, carry: bool) -> Vec<String> {
    let mut said = Vec::new();
    let names = super::names_in(project);
    if !carry {
        for name in &names {
            let at = project.join(name);
            if let Err(e) = std::fs::remove_file(&at) {
                said.push(format!("{}: {e}", at.display()));
            }
        }
        return said;
    }
    for name in &names {
        let at = project.join(name);
        // A link in a copy is removed on either switch: it points at something of THIS machine's,
        // which a guest does not have, and `set_permissions` would chmod that something.
        let link = std::fs::symlink_metadata(&at)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if link {
            if let Err(e) = std::fs::remove_file(&at) {
                said.push(format!("{}: {e}", at.display()));
            }
            continue;
        }
        if let Err(why) = set_private(&at) {
            said.push(why);
        }
    }
    if let Err(why) = exclude_local(project, &names) {
        said.push(why);
    }
    said
}

/// Append `names` to the tree's `.git/info/exclude`, skipping the ones git already ignores. A tree
/// that is not a repository is not an error: there is nothing there to commit.
pub fn exclude_local(wt: &Path, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    let Ok(printed) = crate::git::git(wt, &["rev-parse", "--git-path", "info/exclude"]) else {
        return Ok(());
    };
    let printed = printed.trim();
    if printed.is_empty() {
        return Ok(());
    }
    let file = match std::path::Path::new(printed).is_absolute() {
        true => PathBuf::from(printed),
        false => wt.join(printed),
    };
    let mut add = Vec::new();
    for name in names {
        // `check-ignore -q` answers 0 for ignored and 1 for not ignored; anything else is a tree
        // git cannot read, which is not "not ignored" and is skipped rather than papered over.
        match exit_of(wt, &["check-ignore", "-q", "--", name]) {
            Ok(Some(0)) => {}
            Ok(Some(1)) => add.push(name.clone()),
            _ => {}
        }
    }
    if add.is_empty() {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let had = std::fs::read_to_string(&file).unwrap_or_default();
    let mut out = had.clone();
    for name in add {
        // Git's own `info/exclude` carries commented examples on a fresh clone, so a line is a
        // pattern only when it is neither blank nor a comment.
        if had.lines().any(|l| !l.starts_with('#') && l.trim() == name) {
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&name);
        out.push('\n');
    }
    std::fs::write(&file, out).map_err(|e| format!("{}: {e}", file.display()))
}

/// [`exclude_local`] for a tree on another machine, as one `sh` script: the same rule, said where
/// the tree is. Empty when there is nothing to add.
pub fn exclude_script(wt: &str, names: &[String]) -> String {
    let mut out = String::new();
    for name in names {
        out.push_str(&format!(
            "p=$(git -C {wt} rev-parse --git-path info/exclude) || exit 1\n\
             case \"$p\" in /*) ;; *) p={wt}/\"$p\" ;; esac\n\
             mkdir -p \"$(dirname \"$p\")\" || exit 1\n\
             if ! git -C {wt} check-ignore -q -- {name} 2>/dev/null; then\n\
             \x20 grep -qxF {name} \"$p\" 2>/dev/null || echo {name} >> \"$p\"\n\
             fi\n",
            wt = crate::remote::quote(wt),
            name = crate::remote::quote(name)
        ));
    }
    out
}

/// One git command's exit status, `None` when git could not be run at all.
fn exit_of(dir: &Path, args: &[&str]) -> Result<Option<i32>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", crate::git::words(args)))?;
    Ok(out.status.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "letibot-dotenv-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        dir
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("a file")
            .permissions()
            .mode()
            & 0o777
    }

    /// A repository with one worktree, as `task_start` cuts one.
    fn repo_with_worktree(tag: &str) -> (PathBuf, PathBuf) {
        let root = scratch(tag);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["init", "-q", "-b", "main"]);
        crate::testing::repo_config(&repo);
        std::fs::write(repo.join("README.md"), "the tree\n").expect("a file");
        std::fs::write(repo.join(".gitignore"), "/target\n").expect("an ignore file");
        run(&["add", "."]);
        run(&["commit", "-qm", "one"]);
        let wt = root.join("wt");
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/one",
            &wt.to_string_lossy(),
        ]);
        (repo, wt)
    }

    /// The set `names_in` accepts, and the shapes it must not: `.envrc` is run as code, and a name
    /// that is not a word after `.env.` (a second dot, a slash) is not a file this build names.
    /// `.env.example` IS in the set - it is `.env.<name>`, and a template that is tracked never
    /// travels anyway, because only files git ignores or does not track are carried.
    #[test]
    fn the_names_that_are_carried() {
        for yes in [
            ".env",
            ".env.local",
            ".env.knative",
            ".env.knative.local",
            ".env.example",
        ] {
            assert!(super::super::is_name(yes), "{yes} is carried");
        }
        for no in [
            ".envrc",
            "env",
            ".env.",
            ".env.a.b",
            ".envrc.local",
            ".env.x/y",
        ] {
            assert!(!super::super::is_name(no), "{no} is not");
        }
    }

    /// The source's own `.env` reaches the worktree at 0600, and git does not see it.
    #[test]
    fn a_worktree_gets_the_files_and_git_ignores_them() {
        let (repo, wt) = repo_with_worktree("carry-wt");
        std::fs::write(repo.join(".env"), "KO_DOCKER_REPO=kind.local\n").expect("a .env");
        std::fs::write(repo.join(".env.local"), "EXTRA=1\n").expect("a local");
        std::fs::write(repo.join(".envrc"), "export EVIL=1\n").expect("an rc");
        // The source's own tree must not be asked to ignore them; only the copy is.
        let (carried, said) = into_worktree(std::slice::from_ref(&repo), &wt);
        assert_eq!(carried, vec![".env".to_string(), ".env.local".to_string()]);
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).expect("carried"),
            "KO_DOCKER_REPO=kind.local\n"
        );
        assert_eq!(mode(&wt.join(".env")), 0o600);
        assert_eq!(mode(&wt.join(".env.local")), 0o600);
        assert!(!wt.join(".envrc").exists(), ".envrc is never carried");
        // A carried credential is never a commit: the worktree is clean, and the file is listed
        // by the exclude file rather than by the tree's own `.gitignore`.
        let status = crate::git::git(&wt, &["status", "--porcelain"]).expect("git status");
        assert_eq!(status, "", "the worktree is not clean:\n{status}");
        let exclude = std::fs::read_to_string(
            wt.join(
                crate::git::git(&wt, &["rev-parse", "--git-path", "info/exclude"])
                    .expect("the exclude path")
                    .trim(),
            ),
        )
        .expect("the exclude file");
        assert!(exclude.lines().any(|l| l.trim() == ".env"), "{exclude}");
        // The exclude file is the REPOSITORY's, so a linked worktree shares it; a name git already
        // ignores in the worktree is not added on top of that rule.
        let _ = std::fs::remove_dir_all(wt.parent().expect("a root"));
    }

    /// A name git already ignores is not added twice, and one it does not ignore is. The worktree
    /// has to be cut AFTER the ignore rule lands, or it is on a branch that never had it and every
    /// name reads as unignored - which is a fact about the fixture, not about the rule.
    #[test]
    fn an_ignored_file_is_left_alone_and_a_new_one_is_listed() {
        let root = scratch("carry-exclude");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"]);
        crate::testing::repo_config(&repo);
        std::fs::write(repo.join("README.md"), "the tree\n").expect("a file");
        std::fs::write(repo.join(".gitignore"), "/target\n.env.local\n").expect("an ignore");
        run(&["add", "."]);
        run(&["commit", "-qm", "one"]);
        let wt = root.join("wt");
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/one",
            &wt.to_string_lossy(),
        ]);
        std::fs::write(repo.join(".env"), "A=1\n").expect("a .env");
        std::fs::write(repo.join(".env.local"), "B=2\n").expect("a local");
        let (carried, said) = into_worktree(std::slice::from_ref(&repo), &wt);
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(carried.len(), 2);
        let printed = crate::git::git(&wt, &["rev-parse", "--git-path", "info/exclude"])
            .expect("a path")
            .trim()
            .to_string();
        let file = match std::path::Path::new(&printed).is_absolute() {
            true => PathBuf::from(&printed),
            false => wt.join(&printed),
        };
        let exclude = std::fs::read_to_string(&file).expect("the exclude file");
        // Git writes its own commented examples into a fresh `info/exclude`, so a LISTED pattern is
        // a line that is neither blank nor a comment.
        let listed: Vec<&str> = exclude
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(listed, vec![".env"], "only the unignored name is added");
        let _ = std::fs::remove_dir_all(wt.parent().expect("a root"));
    }

    /// A `.env` symlinked at a credential store is not a file to copy.
    #[test]
    fn a_symlink_is_refused_and_named() {
        let (repo, wt) = repo_with_worktree("carry-link");
        let secret = repo.join("real-secret");
        std::fs::write(&secret, "TOKEN=1\n").expect("a secret");
        std::os::unix::fs::symlink(&secret, repo.join(".env")).expect("a link");
        let (carried, said) = into_worktree(std::slice::from_ref(&repo), &wt);
        assert!(carried.is_empty(), "{carried:?}");
        assert_eq!(said.len(), 1);
        assert!(said[0].contains(".env"), "{said:?}");
        assert!(!wt.join(".env").exists());
        let _ = std::fs::remove_dir_all(wt.parent().expect("a root"));
    }

    /// A VM's copy keeps them only when it was told to, and then at 0600 and excluded.
    #[test]
    fn a_guest_copy_keeps_them_only_when_carried() {
        let root = scratch("carry-vm");
        let project = root.join("copy");
        std::fs::create_dir_all(&project).expect("a copy");
        std::fs::write(project.join(".env"), "A=1\n").expect("a .env");
        std::fs::write(project.join(".env.local"), "B=2\n").expect("a local");
        assert!(settle_vm_copy(&project, false).is_empty());
        assert!(!project.join(".env").exists(), "not carried is not there");
        assert!(!project.join(".env.local").exists());

        std::fs::write(project.join(".env"), "A=1\n").expect("a .env");
        std::fs::write(project.join(".env.local"), "B=2\n").expect("a local");
        assert!(settle_vm_copy(&project, true).is_empty());
        assert_eq!(mode(&project.join(".env")), 0o600);
        assert_eq!(mode(&project.join(".env.local")), 0o600);
        let _ = std::fs::remove_dir_all(&root);
    }
}
