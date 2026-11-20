//! Whether a directory is in use and must be kept: a session open on it in any letibot daemon on
//! the box (`session_writer` joined to `session`), a job of such a session still running by its
//! own record, or any process with its cwd inside it (`docs/lifecycle.md` d.4 rule 7).

use std::path::Path;

use letibot_tokencore::store::{ProcessRecord, Store};

use super::autoarchive::Here;

/// Why `dir` is kept, or `None` when nothing is using it. A store that cannot be read is a reason
/// to keep (d.4 rule 8).
pub fn why_kept(dir: &Path, store: Option<&Store>, here: &Here) -> Option<String> {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    // Seat pids the store names: their job rows are read below, by the record that names the cwd.
    let mut known: Vec<u32> = Vec::new();
    let kept = store.and_then(|store| {
        live_session(store, dir, &real, here, &mut known)
            .or_else(|| running_job(store, dir, &real, &mut known))
    });
    let cwd = cwd_report(&real, &here.proc_root, &known);
    let mut why = match (kept, cwd.inside) {
        (Some(why), _) => why,
        (None, Some(why)) => why,
        // A process whose cwd cannot be read is UNKNOWN, not in the tree: another agent's `sleep`
        // in its own mount namespace kept every worktree on a shared box, and took the blame for
        // it. Nothing is kept for them alone; what was seen is said on a tree kept for a real
        // reason, so a reader knows the check was blind for that many processes.
        (None, None) => return None,
    };
    if !cwd.unknown.is_empty() {
        why.push_str(&format!(
            "; {} process(es) of ours have a cwd this box cannot read, so they are unknown rather \
             than counted",
            cwd.unknown.len()
        ));
    }
    Some(why)
}

fn live_session(
    store: &Store,
    dir: &Path,
    real: &Path,
    here: &Here,
    known: &mut Vec<u32>,
) -> Option<String> {
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
                known.push(w.pid);
                return Some(format!(
                    "the session `{}` is open in the daemon at pid {}",
                    w.session_id, w.pid
                ));
            }
        }
    }
    None
}

/// A job of a session that works in this tree, proved by its own record and a live pid. The
/// writer row can be gone with its daemon while the job still runs on.
fn running_job(store: &Store, dir: &Path, real: &Path, known: &mut Vec<u32>) -> Option<String> {
    let mut roots = vec![dir.display().to_string()];
    if real != dir {
        roots.push(real.display().to_string());
    }
    for root in roots {
        let ids = match store.session_ids_under(&root) {
            Ok(ids) => ids,
            Err(e) => {
                return Some(format!("whether a job runs in it could not be read: {e}"));
            }
        };
        for id in ids {
            for p in open_processes_of(store, &id) {
                let Some(start) = p
                    .identity
                    .strip_prefix("start:")
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    continue;
                };
                if !crate::exec::procs::alive(p.pid, start) {
                    continue;
                }
                known.push(p.pid);
                if Path::new(&p.cwd).starts_with(dir) || Path::new(&p.cwd).starts_with(real) {
                    return Some(format!(
                        "the session `{id}` has its job `{}` running in it (pid {})",
                        p.handle, p.pid
                    ));
                }
            }
        }
    }
    None
}

/// The open process rows a store holds for one session: its own routed `process` table, and every
/// project file of a split set, because the rows live in the file of the daemon that ran them
/// (`open_job_elsewhere`'s reason one question over).
fn open_processes_of(store: &Store, id: &str) -> Vec<ProcessRecord> {
    let mut out = store
        .processes_of(id)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.state.is_open() && p.host.is_none())
        .collect::<Vec<_>>();
    let Some(dir) = store
        .is_set()
        .then(|| store.path().and_then(Path::parent))
        .flatten()
    else {
        return out;
    };
    let projects = dir.join(letibot_tokencore::store::split::PROJECTS_DIR);
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return out;
    };
    for f in entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "db"))
    {
        if let Ok(s) = Store::open_reader(&f) {
            out.extend(
                s.processes_of(id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| p.state.is_open() && p.host.is_none()),
            );
        }
    }
    out
}

/// What the process table says about `real`: the first process with its cwd inside it, and the
/// pids whose cwd cannot be read at all.
struct Cwds {
    inside: Option<String>,
    unknown: Vec<u32>,
}

/// The processes whose cwd is `real` or inside it, and the ones that could not be judged. An
/// unlistable `/proc` keeps, and a missing one is a box without the check.
fn cwd_report(real: &Path, proc_root: &Path, known: &[u32]) -> Cwds {
    let mut out = Cwds {
        inside: None,
        unknown: Vec::new(),
    };
    let entries = match std::fs::read_dir(proc_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.inside = Some(format!(
                "whether a process has its cwd in it could not be told: {e}"
            ));
            return out;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let cwd = match std::fs::read_link(entry.path().join("cwd")) {
            Ok(cwd) => cwd,
            // Ours, alive, and unreadable: a different mount namespace, a non-dumpable process,
            // a pid on its way out. Which of those it is cannot be told from here, so it is
            // UNKNOWN - recorded, and never a reason on its own.
            Err(_)
                if !is_zombie(&entry.path())
                    && (known.contains(&pid) || owned_by_us(&entry.path())) =>
            {
                out.unknown.push(pid);
                continue;
            }
            Err(_) => continue,
        };
        if cwd.starts_with(real) {
            let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            out.inside = Some(format!(
                "pid {pid} ({}) has its cwd in it ({})",
                comm.trim(),
                cwd.display()
            ));
            break;
        }
    }
    out
}

/// Is this /proc entry ours by uid? A foreign user's unreadable cwd is not a keep.
fn owned_by_us(entry: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(entry), std::fs::metadata("/proc/self")) {
        (Ok(a), Ok(b)) => a.uid() == b.uid(),
        _ => false,
    }
}

/// Is the process behind this entry dead and unreaped? A zombie's cwd link is unreadable because
/// it is gone, not because we may not see where it sits.
fn is_zombie(entry: &Path) -> bool {
    let Ok(stat) = std::fs::read_to_string(entry.join("stat")) else {
        return false;
    };
    // `pid (comm) state …` - comm may hold spaces and parens, so split at the LAST `)`.
    stat.rfind(')')
        .and_then(|close| stat[close + 1..].trim_start().chars().next())
        .is_some_and(|state| state == 'Z')
}

#[cfg(test)]
mod tests {
    use super::*;
    use letibot_tokencore::store::{ProcState, ProcessRecord};

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

    /// A /proc that cannot be listed (hidepid, or a view with no read on it) is a reason to keep
    /// the tree, not a silent "nothing is using it".
    #[test]
    fn why_kept_keeps_when_proc_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::backend::tempdir::TempDir::new();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let proc = dir.path().join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        std::fs::set_permissions(&proc, std::fs::Permissions::from_mode(0o000)).unwrap();
        let here = Here {
            host: crate::lifecycle::box_id(),
            pid: std::process::id(),
            proc_root: proc.clone(),
        };
        let why = why_kept(&wt, None, &here);
        let _ = std::fs::set_permissions(&proc, std::fs::Permissions::from_mode(0o700));
        let why = why.expect("an unreadable /proc is a reason to keep");
        assert!(why.to_lowercase().contains("could not"), "{why}");
    }

    /// A process of our own uid whose cwd link cannot be read is UNKNOWN, not in the tree: another
    /// agent's `sleep` in its own mount namespace kept every worktree on a shared box and took the
    /// blame for it, so an unreadable cwd is never a reason on its own - and when a tree IS kept,
    /// the reason says how many processes the check could not judge.
    #[test]
    fn an_unreadable_cwd_is_unknown_and_never_keeps_the_tree_alone() {
        let dir = crate::backend::tempdir::TempDir::new();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let proc = dir.path().join("proc");
        std::fs::create_dir_all(proc.join("4242")).unwrap();
        // `cwd` as a regular file: read_link fails on it the way it fails on another user's link.
        std::fs::write(proc.join("4242/cwd"), "not a link").unwrap();
        let here = Here {
            host: crate::lifecycle::box_id(),
            pid: std::process::id(),
            proc_root: proc.clone(),
        };
        assert_eq!(
            why_kept(&wt, None, &here),
            None,
            "an unreadable cwd is not a reason to keep"
        );

        // A second process whose cwd IS in the tree keeps it, and the sentence says what could
        // not be judged rather than blaming 4242 for it.
        std::fs::create_dir_all(proc.join("4343")).unwrap();
        std::fs::write(proc.join("4343/comm"), "sleep\n").unwrap();
        std::os::unix::fs::symlink(&wt, proc.join("4343/cwd")).unwrap();
        let why = why_kept(&wt, None, &here).expect("a cwd inside still keeps it");
        assert!(why.contains("pid 4343"), "{why}");
        assert!(
            why.contains("1 process(es) of ours have a cwd this box cannot read"),
            "{why}"
        );
    }

    /// A running job of a session whose workspace is the tree keeps it, on the store's own
    /// record: the writer row can be gone with the daemon while the job still runs.
    #[test]
    fn a_running_job_of_a_session_in_the_tree_keeps_it() {
        let dir = crate::backend::tempdir::TempDir::new();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        let here = Here::this_process();
        let store = Store::open_in_memory().unwrap();
        session_in(&store, "s-job", &wt.join("src"));
        let mut job = std::process::Command::new("sleep")
            .arg("120")
            .spawn()
            .unwrap();
        let start = crate::exec::procs::read(job.id())
            .expect("the job's start time")
            .start;
        let mut rec = ProcessRecord::starting("s-job", "j1", None, job.id());
        rec.identity = format!("start:{start}");
        rec.cwd = wt.join("src").display().to_string();
        rec.state = ProcState::Running;
        store.put_process(&rec).unwrap();
        let why = why_kept(&wt, Some(&store), &here);
        let _ = job.kill();
        let _ = job.wait();
        let why = why.expect("a running job of a session in the tree keeps it");
        assert!(why.contains("s-job") && why.contains("j1"), "{why}");
        assert_eq!(
            why_kept(&wt, Some(&store), &here),
            None,
            "and with the job gone, it goes"
        );
    }
}
