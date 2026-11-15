//! Whether a directory is in use and must be kept: a session open on it in any letibot daemon on
//! the box (`session_writer` joined to `session`), or any process with its cwd inside it
//! (`docs/lifecycle.md` d.4 rule 7).

use std::path::Path;

use letibot_tokencore::store::Store;

use super::autoarchive::Here;

/// Why `dir` is kept, or `None` when nothing is using it. A store that cannot be read is a reason
/// to keep (d.4 rule 8).
pub fn why_kept(dir: &Path, store: Option<&Store>, here: &Here) -> Option<String> {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Some(store) = store
        && let Some(why) = live_session(store, dir, &real, here)
    {
        return Some(why);
    }
    cwd_inside(&real, &here.proc_root)
}

fn live_session(store: &Store, dir: &Path, real: &Path, here: &Here) -> Option<String> {
    let mut roots = vec![dir.display().to_string()];
    if real != dir {
        roots.push(real.display().to_string());
    }
    for root in roots {
        let writers = match store.writers_under(&root) {
            Ok(w) => w,
            Err(e) => {
                return Some(format!(
                    "whether a session is open in it could not be read: {e}"
                ));
            }
        };
        for w in writers {
            if w.host != here.host {
                return Some(format!(
                    "the session `{}` is held open on {}",
                    w.session_id, w.host
                ));
            }
            if crate::firecode::held::alive(&here.proc_root, w.pid) {
                return Some(format!(
                    "the session `{}` is open in the daemon at pid {}",
                    w.session_id, w.pid
                ));
            }
        }
    }
    None
}

/// The first process whose cwd is `real` or inside it. A process whose cwd cannot be read is
/// another user's, and is not one of ours.
fn cwd_inside(real: &Path, proc_root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(proc_root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(cwd) = std::fs::read_link(entry.path().join("cwd")) else {
            continue;
        };
        if cwd.starts_with(real) {
            let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            return Some(format!(
                "pid {pid} ({}) has its cwd in it ({})",
                comm.trim(),
                cwd.display()
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_in(store: &Store, id: &str, dir: &Path) {
        store
            .put_session(&letibot_tokencore::store::SessionRecord {
                id: id.into(),
                title: None,
                model_id: "m".into(),
                dialect_sha: "sha".into(),
                workspace_root: dir.display().to_string(),
                owner: "dead".into(),
                role: None,
                approvers: vec![],
                parent_session_id: None,
            })
            .unwrap();
    }

    #[test]
    fn another_daemons_live_session_and_a_cwd_inside_keep_a_tree_and_a_dead_one_does_not() {
        let dir = crate::backend::tempdir::TempDir::new();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        let here = Here::this_process();
        let store = Store::open_in_memory().unwrap();
        assert_eq!(why_kept(&wt, Some(&store), &here), None, "nothing uses it");

        // A session working inside the tree, held by another live process on this box.
        let mut daemon = std::process::Command::new("sleep")
            .arg("120")
            .spawn()
            .unwrap();
        session_in(&store, "s-other", &wt.join("src"));
        store
            .claim_session_writer("s-other", "o/1/1", daemon.id(), &here.host, 1, 0)
            .unwrap();
        let why = why_kept(&wt, Some(&store), &here);
        let _ = daemon.kill();
        let _ = daemon.wait();
        let why = why.expect("a live session in another daemon keeps it");
        assert!(
            why.contains("s-other") && why.contains(&daemon.id().to_string()),
            "{why}"
        );
        assert_eq!(
            why_kept(&wt, Some(&store), &here),
            None,
            "a writer row whose process is gone is not a reason"
        );

        // A process with its cwd inside it.
        let mut inside = std::process::Command::new("sleep")
            .arg("120")
            .current_dir(wt.join("src"))
            .spawn()
            .unwrap();
        let why = why_kept(&wt, None, &here);
        let _ = inside.kill();
        let _ = inside.wait();
        let why = why.expect("a cwd inside keeps it");
        assert!(why.contains(&format!("pid {}", inside.id())), "{why}");
        assert_eq!(
            why_kept(&wt, Some(&store), &here),
            None,
            "and gone, it goes"
        );

        // A sibling whose name only starts the same is not inside.
        let sibling = dir.path().join("wt-2");
        std::fs::create_dir_all(&sibling).unwrap();
        let mut beside = std::process::Command::new("sleep")
            .arg("120")
            .current_dir(&sibling)
            .spawn()
            .unwrap();
        let why = why_kept(&wt, None, &here);
        let _ = beside.kill();
        let _ = beside.wait();
        assert_eq!(why, None);
    }
}
