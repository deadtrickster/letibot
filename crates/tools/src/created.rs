//! What a command CREATED on the host: a path that was not there before the call and is after it,
//! under a place the command could write. Such a path is the session's own work - its `git worktree
//! add`, its `git clone`, its `mkdir -p` - so it joins the session's view read-write without a card.
//! Four cards for one tree the child had just built is the boundary asking about its own work.
//!
//! The measurement is a directory's own listing: a new entry at a place the command could write was
//! created by it, and a directory whose mtime moved has something new inside it, so only the
//! subtrees the command touched are read. What it cannot see is named on [`Created::created_since`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How many directory entries one call may read while looking for creations: a command that rewrites
/// a whole tree should cost a bounded walk, and [`New::capped`] says when it was cut short.
pub const MAX_LISTED: usize = 4_000;

/// How deep the descent into a changed directory goes. A `git clone` makes its tree whole, so the
/// top of it is enough; a deeper path is a path this build does not see.
pub const MAX_DEPTH: usize = 8;

/// What a look for creations found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct New {
    /// The paths this command created, each the top of a tree rather than every file in it.
    pub paths: Vec<PathBuf>,
    /// The look stopped at [`MAX_LISTED`] entries, so a creation past that is not in `paths`.
    pub capped: bool,
}

/// One directory as it was before the call: its entries, and each one's kind and mtime.
#[derive(Debug, Clone, Default)]
struct Listed {
    entries: BTreeMap<OsString, (bool, Option<SystemTime>)>,
}

/// The places a command could write, as they stood before it ran.
#[derive(Debug, Clone, Default)]
pub struct Created {
    roots: Vec<(PathBuf, Listed)>,
    /// The wall clock the snapshot was taken at: an entry inside a changed directory is this
    /// command's when its mtime is at or after it.
    since: Option<SystemTime>,
}

impl Created {
    /// Take the snapshot: one `read_dir` per place, and nothing recursive.
    pub fn before(places: &[PathBuf]) -> Created {
        Created {
            since: Some(SystemTime::now()),
            roots: places.iter().map(|p| (p.clone(), list(p))).collect(),
        }
    }

    /// The paths this command created. A new entry at a place the command could write is one, and so
    /// is anything a directory whose mtime moved holds that was written at or after the snapshot. A
    /// new DIRECTORY is taken whole: a `git clone` makes its tree, and every file in it is new work.
    ///
    /// What this cannot see, and the report says rather than hiding: a creation under a place the
    /// command could write that was not among the places (a path outside the view and not a bind
    /// source); anything a container wrote where the session gave it no `-v`; a creation by a
    /// BACKGROUND command, whose scan would have to run when the job ends rather than when the call
    /// returns; a write INTO an existing file (only new paths are found); and a directory too deep
    /// ([`MAX_DEPTH`]) or a listing cut short ([`MAX_LISTED`]).
    pub fn created_since(&self) -> New {
        let mut out = New::default();
        let mut budget = MAX_LISTED;
        for (root, before) in &self.roots {
            let now = list(root);
            for (name, (is_dir, mtime)) in &now.entries {
                if !spend(&mut budget) {
                    out.capped = true;
                    return out;
                }
                match before.entries.get(name) {
                    // A place the command could write has an entry this call put there.
                    None => out.paths.push(root.join(name)),
                    Some((was_dir, was_mtime)) if *is_dir && *was_dir && mtime != was_mtime => {
                        self.descend(&root.join(name), 1, &mut budget, &mut out);
                    }
                    Some(_) => {}
                }
                if budget == 0 {
                    out.capped = true;
                    return out;
                }
            }
        }
        out
    }

    /// Look inside a directory whose mtime moved for an entry this call made. Only entries stamped
    /// at or after the snapshot are taken, so another session's older work under the same directory
    /// is not claimed; a directory older than the call is descended into, one level at a time.
    fn descend(&self, dir: &Path, depth: usize, budget: &mut usize, out: &mut New) {
        if depth > MAX_DEPTH {
            out.capped = true;
            return;
        }
        let Some(since) = self.since else {
            return;
        };
        for (name, (is_dir, mtime)) in list(dir).entries {
            if !spend(budget) {
                out.capped = true;
                return;
            }
            let at = dir.join(&name);
            match mtime {
                Some(t) if t >= since => out.paths.push(at),
                // Older than this call, so it was there before: only what it holds can be new.
                _ if is_dir => self.descend(&at, depth + 1, budget, out),
                _ => {}
            }
        }
    }
}

/// One entry of the budget, and whether there was one left.
fn spend(budget: &mut usize) -> bool {
    *budget = budget.saturating_sub(1);
    *budget > 0
}

/// One directory's entries, bounded by [`MAX_LISTED`].
fn list(dir: &Path) -> Listed {
    let mut listed = Listed::default();
    let Ok(read) = std::fs::read_dir(dir) else {
        return listed;
    };
    for (i, entry) in read.flatten().enumerate() {
        if i >= MAX_LISTED {
            break;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        listed
            .entries
            .insert(entry.file_name(), (meta.is_dir(), meta.modified().ok()));
    }
    listed
}

/// The host paths a command hands a container to write, from `docker run -v SRC:DST`,
/// `--volume SRC:DST`, `--volume=SRC:DST` and `--mount ...,source=SRC,...`.
///
/// These count as places the command could write and not as grants of their own: what a container
/// creates under one is the session's own work, and what the session may reach is still the view it
/// had. A `-v` naming a docker VOLUME rather than a path is skipped, because a name is not a place.
pub fn bind_sources(command: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let tokens: Vec<&str> = command.split_whitespace().collect();
    // Only after a word that IS a container client: `cp -v` and `tar -v` name no bind mount.
    let Some(from) = tokens.iter().position(|t| {
        let word = t.rsplit('/').next().unwrap_or(t);
        matches!(word, "docker" | "podman" | "nerdctl")
    }) else {
        return out;
    };
    let mut i = from + 1;
    while i < tokens.len() {
        let t = tokens[i];
        let value = if t == "-v" || t == "--volume" || t == "--mount" {
            i += 1;
            tokens.get(i).copied()
        } else {
            t.strip_prefix("--volume=")
                .or_else(|| t.strip_prefix("--mount="))
        };
        if let Some(v) = value
            && let Some(src) = host_path_of(v)
            && !out.contains(&src)
        {
            out.push(src);
        }
        i += 1;
    }
    out
}

/// The host side of one `-v`/`--mount` value: a bind source, and nothing else.
fn host_path_of(value: &str) -> Option<PathBuf> {
    // `--mount`'s own grammar: `type=bind,source=/x,target=/y`, in any order and any spelling.
    if value.contains('=') {
        let parts: Vec<(&str, &str)> = value
            .split(',')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let kind = parts.iter().find(|(k, _)| *k == "type").map(|(_, v)| *v);
        let source = parts
            .iter()
            .find(|(k, _)| *k == "source" || *k == "src")
            .map(|(_, v)| *v)?;
        let path = Path::new(source);
        return (path.is_absolute() && kind.is_none_or(|k| k == "bind"))
            .then(|| path.to_path_buf());
    }
    let (src, _dst) = value.split_once(':')?;
    let path = Path::new(src);
    path.is_absolute().then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory to watch, and the guard that removes it when the test ends.
    fn place(tag: &str) -> (crate::backend::tempdir::TempDir, PathBuf) {
        let dir = crate::backend::tempdir::TempDir::new();
        let at = dir.path().join(tag);
        std::fs::create_dir_all(&at).expect("a directory");
        (dir, at.canonicalize().expect("a real directory"))
    }

    /// A directory's mtime moves when a child appears, and that is the whole measurement: the roots
    /// are listed, then only what they say has changed is read.
    #[test]
    fn a_tree_created_after_the_snapshot_is_found_under_a_place_the_command_could_write() {
        let (_guard, root) = place("created");
        std::fs::create_dir(root.join("tmp")).expect("an older child");
        let before = Created::before(std::slice::from_ref(&root));
        std::fs::create_dir(root.join("tmp/leakidx-wt")).expect("the worktree the command made");
        std::fs::write(root.join("tmp/leakidx-wt/README"), "x\n").expect("a file inside it");
        assert_eq!(
            before.created_since(),
            New {
                paths: vec![root.join("tmp/leakidx-wt")],
                capped: false,
            },
            "the new tree, and not every file inside it"
        );
    }

    /// What was there before the call is nobody's creation, however the call rewrote it.
    #[test]
    fn an_entry_that_was_already_there_is_not_a_creation() {
        let (_guard, root) = place("older");
        std::fs::write(root.join("f.txt"), "one\n").expect("a file");
        let before = Created::before(std::slice::from_ref(&root));
        std::fs::write(root.join("f.txt"), "two\n").expect("rewritten, not created");
        assert_eq!(before.created_since(), New::default());
    }

    /// A place that was not there at the snapshot is not a place to read afterwards: the command can
    /// have removed it, and a missing root is not a creation.
    #[test]
    fn a_place_that_is_gone_afterwards_reports_nothing() {
        let (_guard, root) = place("gone");
        let before = Created::before(std::slice::from_ref(&root));
        std::fs::remove_dir_all(&root).expect("gone");
        assert_eq!(before.created_since(), New::default());
    }

    /// The same rule for a tree a container built: the source the session handed `docker run -v` is
    /// a place the command could write, so what appeared under it is the session's own.
    #[test]
    fn a_bind_source_is_a_place_the_command_could_write() {
        let (_guard, root) = place("bind");
        let before = Created::before(std::slice::from_ref(&root));
        std::fs::create_dir(root.join("worktree")).expect("the tree a container made");
        assert_eq!(before.created_since().paths, vec![root.join("worktree")]);
    }

    /// The four spellings of a bind mount, and the two things that are not one.
    #[test]
    fn a_bind_mount_is_read_off_the_command_line() {
        assert_eq!(
            bind_sources(
                "docker run --rm -v /build/tmp:/h -w /h alpine sh -c 'git worktree add wt main'"
            ),
            vec![PathBuf::from("/build/tmp")]
        );
        assert_eq!(
            bind_sources(
                "docker run --volume=/build/x:/y --mount type=bind,source=/build/z,target=/z img"
            ),
            vec![PathBuf::from("/build/x"), PathBuf::from("/build/z")]
        );
        assert_eq!(
            bind_sources("/usr/bin/podman run --volume /build/q:/q img"),
            vec![PathBuf::from("/build/q")]
        );
        // A volume's name is not a path, and a `cp -v` is not a container.
        assert_eq!(
            bind_sources("docker run -v mydata:/data img"),
            Vec::<PathBuf>::new()
        );
        assert_eq!(bind_sources("cp -v a b"), Vec::<PathBuf>::new());
        assert_eq!(bind_sources("tar -v -f x.tar"), Vec::<PathBuf>::new());
    }
}
