//! **The repository's own references, made links** — the operator: *"any chance to infer github
//! branch names and prs?"*.
//!
//! A word on the screen that names something in the session's GitHub repository becomes a link to
//! it (OSC 8, where the terminal speaks it): `owner/repo#12` always (it says which repository),
//! `#12` in the session's repository, a commit hash this repository has, a branch name it has. What
//! is checked is what the repository itself holds — its `origin`, its refs, its commits — read in
//! the background like the header's git field ([`App::refresh_github_refs`]), so a guess never
//! becomes a link: random hex is not a commit, and a word that only looks like a branch is not one.

use std::collections::HashSet;

use super::*;

/// How often the references are read again.
const REFS_EVERY_MS: u64 = 60_000;
/// How many commits are kept to recognise a hash by — the newest, every ref's history.
const COMMITS_KEPT: usize = 20_000;

/// **What the session's repository holds, as words can name it.**
#[derive(Debug, Clone, Default)]
pub(crate) struct GitRefs {
    /// `owner/repo` on GitHub.
    pub(crate) slug: String,
    /// Branch names, local and remote (`origin/x` and `x` both).
    pub(crate) branches: HashSet<String>,
    /// Full commit hashes, sorted, for a prefix lookup.
    pub(crate) commits: Vec<String>,
}

fn git(dir: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// **`owner/repo` of a GitHub remote URL** — `https://github.com/o/r(.git)` or
/// `git@github.com:o/r(.git)`; `None` for any other host.
pub(crate) fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| url.trim().strip_prefix("git@github.com:"))
        .or_else(|| url.trim().strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (o, r) = (parts.next()?, parts.next()?);
    (!o.is_empty() && !r.is_empty() && parts.next().is_none()).then(|| format!("{o}/{r}"))
}

/// **Read a repository's references** — `None` when it is not a GitHub repository.
pub(crate) fn read_refs(dir: &str) -> Option<GitRefs> {
    let slug = github_slug(&git(dir, &["remote", "get-url", "origin"])?)?;
    let mut branches = HashSet::new();
    for b in git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .unwrap_or_default()
    .lines()
    {
        let b = b.trim();
        if b.is_empty() || b.ends_with("/HEAD") {
            continue;
        }
        if let Some(short) = b.strip_prefix("origin/") {
            branches.insert(short.to_string());
        }
        branches.insert(b.to_string());
    }
    let mut commits: Vec<String> = git(
        dir,
        &["rev-list", "--all", &format!("--max-count={COMMITS_KEPT}")],
    )
    .unwrap_or_default()
    .lines()
    .map(str::to_string)
    .collect();
    commits.sort();
    Some(GitRefs {
        slug,
        branches,
        commits,
    })
}

impl GitRefs {
    /// **The link a word on the screen names**, or `None` for a word this repository does not
    /// know. Allocates only for a word it links.
    pub(crate) fn url_of(&self, word: &str) -> Option<String> {
        // `owner/repo#12` — says which repository, so it is linked whatever this one is.
        if let Some((repo, n)) = word.rsplit_once('#')
            && repo.contains('/')
            && repo.split('/').count() == 2
            && !n.is_empty()
            && n.len() <= 7
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            return Some(format!("https://github.com/{repo}/issues/{n}"));
        }
        // `#12` — this repository's.
        if let Some(n) = word.strip_prefix('#')
            && !n.is_empty()
            && n.len() <= 7
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            return Some(format!("https://github.com/{}/issues/{n}", self.slug));
        }
        // A commit this repository has — by a prefix of at least seven, matching one commit.
        if (7..=40).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_hexdigit()) {
            let w = word.to_ascii_lowercase();
            let at = self.commits.partition_point(|c| c.as_str() < w.as_str());
            let mut hits = self.commits[at..].iter().take_while(|c| c.starts_with(&w));
            if let (Some(full), None) = (hits.next(), hits.next()) {
                return Some(format!("https://github.com/{}/commit/{full}", self.slug));
            }
            return None;
        }
        // A branch this repository has — only a name with a `/` in it (`agent/fix`): a bare `main`
        // on every line of prose is not a link anybody wanted.
        if word.contains('/') && self.branches.contains(word) {
            let name = word.strip_prefix("origin/").unwrap_or(word);
            return Some(format!("https://github.com/{}/tree/{name}", self.slug));
        }
        None
    }
}

impl App {
    /// **Keep the repository's references current**, read off the UI thread — the repository the
    /// header's git field reads (the followed one in a workspace of several), every
    /// [`REFS_EVERY_MS`].
    pub(crate) fn refresh_github_refs(&mut self) {
        if !self.features.links {
            return;
        }
        let ws = self.wiring.workspace.clone();
        let dir = match &self.git_dir {
            Some(d) => d.display().to_string(),
            None if !ws.is_empty() && self.git_repos.len() <= 1 => ws,
            None => {
                self.gh_refs = None;
                return;
            }
        };
        if let Some((of, rx)) = &self.gh_job {
            match rx.try_recv() {
                Ok(refs) => {
                    if of == &dir {
                        self.gh_refs = refs.map(std::sync::Arc::new);
                        self.gh_read = (dir.clone(), self.now_ms);
                        self.redraw = true;
                    }
                    self.gh_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.gh_job = None,
            }
        }
        let (last, at) = &self.gh_read;
        if last == &dir && self.now_ms.saturating_sub(*at) < REFS_EVERY_MS {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let d = dir.clone();
        if std::thread::Builder::new()
            .name("gh-refs".into())
            .spawn(move || {
                let _ = tx.send(read_refs(&d));
            })
            .is_ok()
        {
            self.gh_job = Some((dir.clone(), rx));
            self.gh_read = (dir, self.now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs() -> GitRefs {
        GitRefs {
            slug: "stroppy-io/stroppy".into(),
            branches: [
                "agent/fix-x",
                "origin/agent/fix-x",
                "main",
                "fix/banking-ydb-unique-index",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            commits: vec![
                "0aabbccddeeff00112233445566778899aabbcc".into(),
                "68040b5aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            ],
        }
    }

    #[test]
    fn the_repositorys_own_references_link_and_guesses_do_not() {
        let r = refs();
        let gh = "https://github.com";
        assert_eq!(
            r.url_of("#212").as_deref(),
            Some(format!("{gh}/stroppy-io/stroppy/issues/212").as_str())
        );
        assert_eq!(
            r.url_of("stroppy-io/stroppy-io.github.io#24").as_deref(),
            Some(format!("{gh}/stroppy-io/stroppy-io.github.io/issues/24").as_str())
        );
        assert_eq!(
            r.url_of("68040b5").as_deref(),
            Some(
                format!("{gh}/stroppy-io/stroppy/commit/68040b5aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                    .as_str()
            )
        );
        assert_eq!(
            r.url_of("deadbeef1"),
            None,
            "hex the repository does not have"
        );
        assert_eq!(
            r.url_of("fix/banking-ydb-unique-index").as_deref(),
            Some(format!("{gh}/stroppy-io/stroppy/tree/fix/banking-ydb-unique-index").as_str())
        );
        assert_eq!(
            r.url_of("origin/agent/fix-x").as_deref(),
            Some(format!("{gh}/stroppy-io/stroppy/tree/agent/fix-x").as_str())
        );
        assert_eq!(r.url_of("main"), None, "a bare name is prose");
        assert_eq!(r.url_of("src/main.rs"), None, "a path is not a branch");
        assert_eq!(r.url_of("#"), None);
    }

    #[test]
    fn a_remote_url_names_its_github_repository() {
        for (u, want) in [
            ("https://github.com/o/r.git\n", Some("o/r")),
            ("git@github.com:o/r.git", Some("o/r")),
            ("ssh://git@github.com/o/r", Some("o/r")),
            ("https://gitlab.com/o/r.git", None),
        ] {
            assert_eq!(github_slug(u).as_deref(), want, "{u}");
        }
    }
}
