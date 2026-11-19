//! What a seat is working on, in the one spelling two boxes can compare: the repository by its
//! IDENTITY - the root commit and where the remote says it lives - never by the path it was
//! checked out at, because the Mac's checkout is not this box's path.
//!
//! A seat's profile is that plus its branch and the one line it is on, which is the whole of what
//! another box's seat needs in order to be told who it now shares a room with.

use std::collections::BTreeSet;

/// Where a repository is, as two boxes can agree on it: `github.com/deadtrickster/letibot@1a2b3c4d5e6f`.
///
/// A repository with no remote is `local@<root commit>`: two clones of one bare path still match,
/// and a folder that is nobody's clone is named by nothing a path could say.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RepoIdentity(String);

impl RepoIdentity {
    /// The identity of a repository whose root commit and remote were read by the caller.
    /// `remote` is what `git config remote.origin.url` answered, empty when there is none.
    pub fn of(root_commit: &str, remote: &str) -> RepoIdentity {
        let root: String = root_commit
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(12)
            .collect();
        let root = if root.is_empty() {
            "unknown".to_string()
        } else {
            root
        };
        match normalise_remote(remote) {
            Some(at) => RepoIdentity(format!("{at}@{root}")),
            None => RepoIdentity(format!("local@{root}")),
        }
    }

    /// An identity a caller already has as text - a profile read back from a peer, or a test's.
    pub fn named(text: &str) -> RepoIdentity {
        RepoIdentity(text.trim().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The folder a reader recognises the repository by: the last segment of its name, without
    /// `.git`. Empty for an identity with no remote, where nothing but the commit is known.
    pub fn label(&self) -> &str {
        let at = self.0.rsplit_once('@').map(|(at, _)| at).unwrap_or("");
        at.rsplit('/').next().unwrap_or("")
    }
}

/// A remote URL as `host/path`, or `None` when it is not one two boxes could agree on.
///
/// The forms a git remote is written in are all one thing: `git@github.com:o/r.git`,
/// `https://github.com/o/r`, `ssh://git@github.com:22/o/r.git`. The user, the scheme, the port and
/// the `.git` are how a box reaches it, not which repository it is - two boxes clone one repository
/// through two of these. A remote that is a PATH is dropped on purpose: a path is exactly the thing
/// lead 71 says an identity may not be, and two boxes never spell one path the same way.
pub fn normalise_remote(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // What is left of the URL after the scheme's own `//`, if it had one.
    let rest: String = match url.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            rest.to_string()
        }
        // `git@host:path` - scp's spelling, which has no scheme and no slashes to split on.
        None => match url.split_once('@') {
            Some((user, rest)) => {
                if user.is_empty() || user.contains('/') {
                    return None;
                }
                let (host, path) = rest.split_once(':')?;
                if host.is_empty() || path.is_empty() {
                    return None;
                }
                format!("{host}/{path}")
            }
            // A bare `host/path` is a remote git writes only for a rewritten URL; a path
            // (`/x`, `./x`, `../x`) is not one.
            None => {
                if url.starts_with('/') || url.starts_with('.') {
                    return None;
                }
                url.to_string()
            }
        },
    };
    // The user, then the host and its optional port, then the path.
    let rest = rest
        .rsplit_once('@')
        .map(|(_, r)| r)
        .unwrap_or(&rest)
        .to_string();
    let (host, path) = rest.split_once('/')?;
    let host = host.split_once(':').map(|(h, _)| h).unwrap_or(host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

/// What one seat is on: its stable id, the repository by identity, its branch, and the line it is
/// working on. Everything here crosses the wire as it is - none of it is a secret and all of it is
/// what a person would say about the seat in one breath.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SeatProfile {
    /// `<host>/<local>`, the id lead 70 mints and nothing else.
    pub id: String,
    /// `github.com/o/r@1a2b3c4d`, or `local@1a2b3c4d` for a repository with no remote.
    #[serde(default)]
    pub repo: String,
    /// The folder a reader recognises the repository by - the identity's last segment.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub branch: String,
    /// The one line the seat is on, as its own task line says it.
    #[serde(default)]
    pub task: String,
}

impl SeatProfile {
    pub fn new(
        id: impl Into<String>,
        repo: &RepoIdentity,
        branch: &str,
        task: &str,
    ) -> SeatProfile {
        let label = repo.label().to_string();
        SeatProfile {
            id: id.into(),
            repo: repo.as_str().to_string(),
            label,
            branch: branch.trim().to_string(),
            task: task.trim().to_string(),
        }
    }

    pub fn identity(&self) -> &str {
        &self.repo
    }

    pub fn host(&self) -> &str {
        self.id.split_once('/').map(|(h, _)| h).unwrap_or("")
    }

    /// The one line another seat is told: `<host>/<id> (<repo>, <branch>) - <task line>`.
    pub fn line(&self) -> String {
        let repo = if self.label.is_empty() {
            self.repo.as_str()
        } else {
            self.label.as_str()
        };
        let branch = if self.branch.is_empty() {
            "no branch"
        } else {
            self.branch.as_str()
        };
        let head = if repo.is_empty() {
            format!("{} ({branch})", self.id)
        } else {
            format!("{} ({repo}, {branch})", self.id)
        };
        match self.task.is_empty() {
            true => head,
            false => format!("{head} - {}", self.task),
        }
    }
}

/// One of this box's projects, as the mesh has to know it: the room a seat lands in, the repository
/// that room is, and whether this project wants to be introduced to its own repository elsewhere.
///
/// The identity is handed in by the coordinator's owner because it is read from git, and the room
/// crate reads no repository: what it does is compare two identities, which is the whole of
/// matching.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectProfile {
    /// The project key a room is named by here.
    pub key: String,
    pub identity: RepoIdentity,
    /// `[peering] introduce = false` in the project's own `leticode.toml`: its seats are not
    /// introduced to the same repository elsewhere, and nobody's are introduced here.
    pub introduce: bool,
}

/// Which peer seats a seat has already been told about, so an introduction is said once.
///
/// Keyed by the seat AND its repository: a seat that moved to another project is a new thing to be
/// told, and one that reconnects on the same one is not news.
#[derive(Debug, Default)]
pub struct Introductions {
    seen: BTreeSet<String>,
}

impl Introductions {
    pub fn new() -> Introductions {
        Introductions::default()
    }

    /// Is this peer's seat news? The first ask is yes, every later one is no.
    pub fn is_new(&mut self, peer: &SeatProfile) -> bool {
        let key = format!("{}|{}", peer.id, peer.repo);
        self.seen.insert(key)
    }

    /// Forget a seat - what a box does when it hears the peer has gone for good, so a seat that
    /// comes back is introduced again.
    pub fn forget(&mut self, id: &str) {
        self.seen.retain(|k| !k.starts_with(&format!("{id}|")));
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_remote_is_one_identity() {
        for url in [
            "git@github.com:deadtrickster/letibot.git",
            "https://github.com/deadtrickster/letibot",
            "https://github.com/deadtrickster/letibot.git",
            "https://git@github.com/deadtrickster/letibot.git",
            "ssh://git@github.com:22/deadtrickster/letibot.git",
            "ssh://git@github.com/deadtrickster/letibot/",
        ] {
            assert_eq!(
                normalise_remote(url).as_deref(),
                Some("github.com/deadtrickster/letibot"),
                "{url}"
            );
        }
        // The host is case-blind; the path is not.
        assert_eq!(
            normalise_remote("https://GitHub.com/deadtrickster/letibot").as_deref(),
            Some("github.com/deadtrickster/letibot")
        );
        // A remote that is a path names a place, not a repository: the Mac's checkout is not this
        // box's, so a path is never an identity.
        for url in [
            "",
            "/home/dead/Projects/letibot",
            "./x",
            "file:///srv/git/x.git",
        ] {
            assert_eq!(normalise_remote(url), None, "{url}");
        }
        // And two local clones of one bare repository still meet on the root commit.
        let a = RepoIdentity::of("1a2b3c4d5e6f7a8b", "/srv/one");
        let b = RepoIdentity::of("1a2b3c4d5e6f7a8b9c0d", "/srv/two");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "local@1a2b3c4d5e6f");
        // A different repository is a different identity, and a different history with one remote
        // is too - the commit is half the answer.
        assert_ne!(
            RepoIdentity::of("aaaaaaaaaaaa", "git@github.com:o/r.git"),
            RepoIdentity::of("bbbbbbbbbbbb", "git@github.com:o/r.git")
        );
        assert_ne!(
            RepoIdentity::of("aaaaaaaaaaaa", "git@github.com:o/r.git"),
            RepoIdentity::of("aaaaaaaaaaaa", "git@github.com:o/other.git")
        );
        assert_eq!(
            RepoIdentity::of("aaaaaaaaaaaa", "git@github.com:o/r.git").label(),
            "r"
        );
    }

    #[test]
    fn a_seat_is_introduced_once_however_often_it_is_heard() {
        let repo = RepoIdentity::of("1a2b3c4d5e6f", "git@github.com:deadtrickster/letibot.git");
        let peer = SeatProfile::new("mac/s-1792", &repo, "main", "provider fixes");
        assert_eq!(peer.line(), "mac/s-1792 (letibot, main) - provider fixes");
        assert_eq!(peer.host(), "mac");
        // An empty branch and task are still one line a person can read.
        assert_eq!(
            SeatProfile::new("mac/s-1", &repo, "", "").line(),
            "mac/s-1 (letibot, no branch)"
        );
        let mut told = Introductions::new();
        assert!(told.is_new(&peer));
        assert!(!told.is_new(&peer));
        assert!(!told.is_new(&peer.clone()), "a reconnect is not news");
        // The same seat on another project IS news: it is working on something else now.
        let other = RepoIdentity::of("ffff", "git@github.com:deadtrickster/rano.git");
        assert!(told.is_new(&SeatProfile::new("mac/s-1792", &other, "main", "")));
        // A seat that left is introduced again when it comes back.
        told.forget("mac/s-1792");
        assert!(told.is_new(&peer));
    }
}
