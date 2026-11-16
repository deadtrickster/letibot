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
    // Seat pids the store names: a cwd link of theirs that cannot be read is a keep, not a skip.
    let mut known: Vec<u32> = Vec::new();
    if let Some(store) = store
        && let Some(why) = live_session(store, dir, &real, here, &mut known)
    {
        return Some(why);
    }
    if let Some(store) = store
        && let Some(why) = running_job(store, dir, &real, &mut known)
    {
        return Some(why);
    }
    cwd_inside(&real, &here.proc_root, &known)
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

/// The first process whose cwd is `real` or inside it. A `/proc` that cannot be listed is a
/// reason to keep; one that does not exist is a box without the check. A process whose cwd cannot
/// be read is kept only when it is ours - by uid, or a pid the store itself names - since a
/// blanket keep would keep every tree.
fn cwd_inside(real: &Path, proc_root: &Path, known: &[u32]) -> Option<String> {
    let entries = match std::fs::read_dir(proc_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(format!(
                "whether a process has its cwd in it could not be told: {e}"
            ));
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let cwd = match std::fs::read_link(entry.path().join("cwd")) {
            Ok(cwd) => cwd,
            // Another user's process is not one of ours, and a zombie of ours is dead - its cwd
            // is gone with it. Ours and alive may be sitting in the tree, and a blanket keep
            // would keep every tree. A process on its way out (its fs released, not yet a
            // zombie) also has no cwd link: re-read it once, and only a process still there and
            // still alive is believed.
            // A non-dumpable process of ours (systemd --user, ssh-agent) that started before the
            // tree existed is not a keep: every box has them, and they would keep every tree.
            Err(_)
                if !is_zombie(&entry.path())
                    && (known.contains(&pid)
                        || (owned_by_us(&entry.path())
                            && !predates(&entry.path(), proc_root, real))) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
                if !entry.path().exists() || is_zombie(&entry.path()) {
                    continue;
                }
                let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
                return Some(format!(
                    "pid {pid} ({}) is ours and its cwd cannot be read: it may be in it",
                    comm.trim()
                ));
            }
            Err(_) => continue,
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

/// Is this /proc entry ours by uid? A foreign user's unreadable cwd is not a keep.
fn owned_by_us(entry: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(entry), std::fs::metadata("/proc/self")) {
        (Ok(a), Ok(b)) => a.uid() == b.uid(),
        _ => false,
    }
}

/// Did the process behind this entry start before `tree` was created? `false` whenever either
/// time cannot be read, so an unknown stays a keep.
fn predates(entry: &Path, proc_root: &Path, tree: &Path) -> bool {
    let Ok(born) = std::fs::metadata(tree).and_then(|m| m.created()) else {
        return false;
    };
    let Ok(stat) = std::fs::read_to_string(entry.join("stat")) else {
        return false;
    };
    // Field 22, `starttime`, in clock ticks since boot; the 20th after the `)` that ends comm.
    let Some(ticks) = stat
        .rfind(')')
        .and_then(|close| stat[close + 1..].split_whitespace().nth(19))
        .and_then(|t| t.parse::<u64>().ok())
    else {
        return false;
    };
    let Some(boot) = std::fs::read_to_string(proc_root.join("stat"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("btime "))
                .and_then(|b| b.trim().parse::<u64>().ok())
        })
    else {
        return false;
    };
    let hz = crate::exec::procs::clock_ticks().max(1);
    let started = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(boot)
        + std::time::Duration::from_millis(ticks * 1000 / hz);
    // A second of slack: `btime` is whole seconds.
    started + std::time::Duration::from_secs(1) < born
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
    /// the tree, not a silent "nothing is using it" (I62).
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

    /// A process of our own uid whose cwd link cannot be read is kept: it may be sitting in the
    /// tree. A blanket keep would keep every tree, so this is the only unreadable link that is.
    #[test]
    fn an_unreadable_cwd_of_our_own_uid_keeps_the_tree() {
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
            proc_root: proc,
        };
        let why = why_kept(&wt, None, &here).expect("an own-uid cwd we cannot read is a keep");
        assert!(
            why.contains("4242") && why.to_lowercase().contains("cannot"),
            "{why}"
        );
    }

    /// An own-uid process whose cwd cannot be read and that started before the tree existed is
    /// not a keep: systemd --user and the agents are on every box, and would keep every tree.
    #[test]
    fn an_unreadable_cwd_older_than_the_tree_does_not_keep_it() {
        let dir = crate::backend::tempdir::TempDir::new();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        if std::fs::metadata(&wt).and_then(|m| m.created()).is_err() {
            return; // no birth time on this filesystem: the keep stands, as above
        }
        let proc = dir.path().join("proc");
        std::fs::create_dir_all(proc.join("4242")).unwrap();
        std::fs::write(proc.join("4242/cwd"), "not a link").unwrap();
        // Booted at the epoch and started 100 ticks later: long before the tree.
        let fields = ["S"; 18].join(" ");
        std::fs::write(
            proc.join("4242/stat"),
            format!("4242 (ssh-agent) {fields} 0 100 0"),
        )
        .unwrap();
        std::fs::write(proc.join("stat"), "cpu 0 0 0 0\nbtime 1\n").unwrap();
        let here = Here {
            host: crate::lifecycle::box_id(),
            pid: std::process::id(),
            proc_root: proc,
        };
        assert_eq!(why_kept(&wt, None, &here), None);
    }

    /// A running job of a session whose workspace is the tree keeps it, on the store's own
    /// record: the writer row can be gone with the daemon while the job still runs (I62).
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
