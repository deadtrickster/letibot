//! `/listen`: which repositories of a folder project the session hears gate news of. The set lives
//! in the store per workspace, so a new session in the folder inherits it, and unions with
//! `[queue] mute` from leticode.toml, which the UI cannot lift.

use std::path::Path;

use letibot_tokencore::store::Store;

use super::{git, is_muted};

/// The workspace key the store files mutes under.
pub fn workspace_key(workspace: &Path) -> String {
    std::fs::canonicalize(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string()
}

/// The effective mute: the file's names plus the store's.
pub fn effective(store: Option<&Store>, workspace: &Path, config: &[String]) -> Vec<String> {
    let mut out = config.to_vec();
    if let Some(s) = store {
        for n in s.queue_mutes(&workspace_key(workspace)).unwrap_or_default() {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// One repository of the workspace: its key and its directory name.
struct Repo {
    key: String,
    name: String,
}

fn repos(workspace: &Path) -> Vec<Repo> {
    super::repositories_of_workspace(workspace)
        .into_iter()
        .map(|key| {
            let name = Path::new(&key)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| key.clone());
            Repo { key, name }
        })
        .collect()
}

/// The directory name of the repository `cwd` is in, for `only this`.
pub fn this_repo(cwd: &Path) -> Option<String> {
    let key = git::repository_key(cwd)?;
    Path::new(&key)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// The names of the workspace's repositories, for completion and refusals.
pub fn repo_names(workspace: &Path) -> Vec<String> {
    repos(workspace).into_iter().map(|r| r.name).collect()
}

enum Step {
    Mute(usize),
    Listen(usize),
    Only(usize),
    All,
}

/// Run one `/listen` line's words. Nothing is applied unless every word resolves. `this` is the
/// repository `only this` means. The bool is whether it worked.
pub fn run(
    store: &Store,
    workspace: &Path,
    config: &[String],
    words: &[String],
    this: Option<&str>,
) -> (Vec<String>, bool) {
    let repos = repos(workspace);
    let ws = workspace_key(workspace);
    let list = || {
        repos
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let find = |w: &str| -> Result<usize, String> {
        let w = if w == "this" { this.unwrap_or(w) } else { w };
        repos
            .iter()
            .position(|r| r.name == w || r.key == w)
            .ok_or_else(|| format!("`{w}` is no repository of this folder: {}", list()))
    };
    let mut steps = Vec::new();
    let mut it = words.iter().map(String::as_str).peekable();
    while let Some(w) = it.next() {
        let step = match w {
            "all" => Ok(Step::All),
            "only" => match it.next() {
                Some(n) => find(n).map(Step::Only),
                None => Err("/listen only REPO - `this` is the repository you are in".into()),
            },
            _ if w.starts_with('-') && w.len() > 1 => find(&w[1..]).map(Step::Mute),
            _ if w.starts_with('+') && w.len() > 1 => find(&w[1..]).and_then(|i| {
                if is_muted(&repos[i].key, config) {
                    Err(format!(
                        "`{}` is muted by `[queue] mute` in leticode.toml - edit the file, \
                         /listen cannot lift it",
                        repos[i].name
                    ))
                } else {
                    Ok(Step::Listen(i))
                }
            }),
            _ => Err(format!(
                "/listen [-REPO] [+REPO] [only REPO|this] [all] - `{w}` is none of those; \
                 repositories: {}",
                list()
            )),
        };
        match step {
            Ok(s) => steps.push(s),
            Err(e) => return (vec![e], false),
        }
    }
    let set = |name: &str, muted: bool| {
        s_set(store, &ws, name, muted);
    };
    for s in steps {
        match s {
            Step::Mute(i) => set(&repos[i].name, true),
            Step::Listen(i) => set(&repos[i].name, false),
            Step::All => {
                let _ = store.clear_queue_mutes(&ws);
            }
            Step::Only(keep) => {
                for (i, r) in repos.iter().enumerate() {
                    set(&r.name, i != keep);
                }
            }
        }
    }
    (show(store, workspace, config), true)
}

fn s_set(store: &Store, ws: &str, name: &str, muted: bool) {
    let _ = store.set_queue_mute(ws, name, muted);
}

/// Each repository, heard or muted, and where a mute comes from.
pub fn show(store: &Store, workspace: &Path, config: &[String]) -> Vec<String> {
    let stored = store
        .queue_mutes(&workspace_key(workspace))
        .unwrap_or_default();
    let repos = repos(workspace);
    if repos.is_empty() {
        return vec!["this folder is not a project of several repositories".into()];
    }
    repos
        .iter()
        .map(|r| {
            let state = if is_muted(&r.key, config) {
                "muted (leticode.toml [queue] mute)".to_string()
            } else if is_muted(&r.key, &stored) {
                "muted (/listen)".to_string()
            } else {
                "listened".to_string()
            };
            format!("{:<24} {state}", r.name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(tag: &str) -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("listen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for n in ["a", "b", "c"] {
            let p = dir.join(n);
            std::fs::create_dir_all(&p).unwrap();
            git::git(&p, &["init", "-q", "-b", "main"]).unwrap();
            git::git(
                &p,
                &[
                    "remote",
                    "add",
                    "origin",
                    &format!("https://github.com/o/{n}.git"),
                ],
            )
            .unwrap();
        }
        (dir.clone(), Store::open(&dir.join("s.db")).unwrap())
    }

    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn mute_unmute_only_this_all_and_refusals() {
        let (dir, st) = folder("verb");
        let none: Vec<String> = Vec::new();
        let ws = &dir;
        let (_, ok) = run(&st, ws, &none, &w("-a -b"), None);
        assert!(ok);
        assert_eq!(effective(Some(&st), ws, &none), ["a", "b"]);
        let (_, ok) = run(&st, ws, &none, &w("+a"), None);
        assert!(ok);
        assert_eq!(effective(Some(&st), ws, &none), ["b"]);
        let (_, ok) = run(&st, ws, &none, &w("only this"), Some("c"));
        assert!(ok);
        assert_eq!(effective(Some(&st), ws, &none), ["a", "b"]);
        let (_, ok) = run(&st, ws, &none, &w("all"), None);
        assert!(ok);
        assert!(effective(Some(&st), ws, &none).is_empty());
        // Unknown names refuse with the repos and apply nothing.
        let (said, ok) = run(&st, ws, &none, &w("-a -zzz"), None);
        assert!(!ok && said[0].contains("a, b, c"), "{said:?}");
        assert!(effective(Some(&st), ws, &none).is_empty());
        // A file's mute cannot be lifted, by name.
        let cfg = w("b");
        let (said, ok) = run(&st, ws, &cfg, &w("+b"), None);
        assert!(!ok && said[0].contains("leticode.toml"), "{said:?}");
        let list = show(&st, ws, &cfg);
        assert!(list[1].contains("leticode.toml"), "{list:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
