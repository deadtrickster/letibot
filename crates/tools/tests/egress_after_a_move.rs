//! A point that moves re-decides what a command's network is. The boundary's argv is built at
//! probe time, so a session narrowed to `read-only` kept the host's network and one moved to
//! `writes-allowed` kept the empty namespace: the decision has to be measured again, not recorded.
//!
//! The helper here is a shim and the scope tree is plain directories, so what is proved is the
//! plumbing - the request is rebuilt and the boundary replaced - rather than the kernel's.

use std::path::PathBuf;
use std::sync::Mutex;

use letibot_tools::backend::{ExecBackend, HostBackend};
use letibot_tools::exec::confine::{self, BWRAP_ENV, Egress, ViewSpec};
use letibot_tools::exec::{
    ExecError, HostProcesses, Migration, Reaping, ScopeId, ScopeKind, ScopeTree,
};

const READBACK: &str = "\
printf 'ns user user:[4026535001]\\n'
printf 'ns mnt mnt:[4026535002]\\n'
printf 'ns pid pid:[4026535003]\\n'
printf 'ns net net:[4026535004]\\n'
printf 'ns ipc ipc:[4026535005]\\n'
printf 'ns uts uts:[4026535006]\\n'
printf 'ns cgroup cgroup:[4026535007]\\n'
printf 'seal nested held\\n'
printf 'seal nnp 1\\n'";

/// `LETIBOT_BWRAP` is one variable for the process, so a case holds it for as long as it probes -
/// twice, once at open and once for the move - and the second case waits its turn.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The turn on the variable, given back when it is dropped.
struct Armed(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Drop for Armed {
    fn drop(&mut self) {
        // SAFETY: the guard this owns is the only thing that sets it, and no thread is started.
        unsafe { std::env::remove_var(BWRAP_ENV) };
    }
}

/// Directories standing in for a cgroup tree. No scope is opened here: this suite asks one
/// question, which is what a `bwrap` argv carries after the point moved.
struct Dirs {
    root: PathBuf,
}

impl ScopeTree for Dirs {
    fn describe(&self) -> String {
        format!("plain directories under {}", self.root.display())
    }

    fn root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn open(
        &self,
        kind: ScopeKind,
        name: &str,
        parent: Option<&ScopeId>,
    ) -> Result<ScopeId, ExecError> {
        let base = parent
            .map(|p| p.path.clone())
            .unwrap_or_else(|| self.root.clone());
        let path = base.join(format!("{}.{name}", kind.as_str()));
        std::fs::create_dir_all(&path)
            .map_err(|e| ExecError::Spawn(format!("{}: {e}", path.display())))?;
        Ok(ScopeId {
            kind,
            name: name.to_string(),
            path,
        })
    }

    fn members(&self, _scope: &ScopeId) -> Result<Vec<u32>, ExecError> {
        Ok(Vec::new())
    }

    fn end(&self, scope: &ScopeId) -> Reaping {
        Reaping {
            scope: scope.clone(),
            at: std::time::SystemTime::now(),
            mechanism: "nothing to kill",
            observed: Vec::new(),
            survivors: Vec::new(),
            waited: std::time::Duration::ZERO,
            removed: std::fs::remove_dir_all(&scope.path).is_ok(),
            note: None,
        }
    }

    fn migrate(&self, _from: &ScopeId, _to: &ScopeId) -> Result<Migration, ExecError> {
        Err(ExecError::Spawn("this tree cannot move a process".into()))
    }

    fn list(&self) -> Vec<ScopeId> {
        Vec::new()
    }
}

/// One rig: a project to hold the view, a helper that answers the readback with namespaces of its
/// own and writes its argv down, and the log it writes to.
struct Rig {
    dir: PathBuf,
    project: PathBuf,
    helper: PathBuf,
    log: PathBuf,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "letibot-egress-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let project = dir.join("project");
        for d in [&project, &dir.join("bin")] {
            std::fs::create_dir_all(d).expect("a directory");
        }
        std::fs::write(project.join("Cargo.toml"), b"[package]\n").expect("a file to see");
        let log = dir.join("argv.log");
        let helper = dir.join("bin").join("bwrap");
        // It runs nothing: the question is what the probe was asked for, not what the kernel did.
        std::fs::write(
            &helper,
            format!(
                "#!/bin/bash\n\
                 [ $# -eq 0 ] && exit 0\n\
                 if [ \"$1\" = \"--version\" ]; then echo 'bubblewrap 0.11.1 (fake)'; exit 0; fi\n\
                 printf 'ARGV %s\\n' \"$*\" >> '{log}'\n\
                 {READBACK}\n\
                 exit 0\n",
                log = log.display()
            ),
        )
        .expect("the shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
            .expect("exec bit");
        Rig {
            dir,
            project,
            helper,
            log,
        }
    }

    /// The argv the helper was last handed, as one line.
    fn last_argv(&self) -> String {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .last()
            .unwrap_or_default()
            .to_string()
    }

    /// The backend over a host whose confinement was measured with `egress`, and that host. The
    /// helper stays in force meanwhile: the move below probes again, and a probe made with the
    /// real `bwrap` on a box that cannot make a namespace would fail for a reason that is the
    /// box's rather than this seam's.
    fn seat(&self, egress: Egress) -> (HostBackend, std::sync::Arc<HostProcesses>, Armed) {
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: no thread is started in this file, and `Armed` restores the variable.
        unsafe { std::env::set_var(BWRAP_ENV, &self.helper) };
        let armed = Armed(guard);
        let measured = confine::probe(ViewSpec::project_only(&self.project), egress)
            .expect("the fake helper answers the probe");
        let host = std::sync::Arc::new(
            HostProcesses::with_tree(
                self.dir.join("tree"),
                Box::new(Dirs {
                    root: self.dir.join("tree"),
                }),
            )
            .with_confinement(measured),
        );
        let backend = HostBackend::executable_with(&self.project, std::sync::Arc::clone(&host))
            .expect("a backend");
        (backend, host, armed)
    }
}

#[test]
fn a_move_that_seats_the_network_probes_the_view_again() {
    let rig = Rig::new("seat");
    let (backend, host, _armed) = rig.seat(Egress::Denied);
    assert!(
        rig.last_argv().contains("--unshare-net"),
        "the first probe was not asked for an empty network: {}",
        rig.last_argv()
    );
    assert!(!host.boundary().expect("a boundary").egress.allowed());

    let said = backend
        .reseat_egress(Egress::Host {
            why: "the point seats network calls".into(),
        })
        .expect("the view is probed again")
        .expect("the network moved, so a probe was spent");
    assert!(said.contains("network"), "it says what it now is: {said}");

    assert_eq!(
        host.boundary().expect("a boundary").egress,
        Egress::Host {
            why: "the point seats network calls".into()
        },
        "the boundary in force is not the one the point decided"
    );
    assert!(
        !rig.last_argv().contains("--unshare-net"),
        "the probe was asked for the same opaque network again: {}",
        rig.last_argv()
    );

    // The same decision again spends nothing: a probe is a process, and a point that did not
    // move its network must not buy one.
    assert_eq!(
        backend
            .reseat_egress(Egress::Host {
                why: "the point seats network calls".into(),
            })
            .expect("nothing to do"),
        None
    );
    let _ = std::fs::remove_dir_all(&rig.dir);
}

/// The other direction is the hazard: a session narrowed after it opened must not keep the host's
/// network because the argv was built before the narrowing.
#[test]
fn a_narrowing_takes_the_network_away_from_the_argv() {
    let rig = Rig::new("narrow");
    let (backend, host, _armed) = rig.seat(Egress::Host {
        why: "the seat it opened at".into(),
    });
    assert!(
        !rig.last_argv().contains("--unshare-net"),
        "the first probe was not asked to share the network"
    );

    backend
        .reseat_egress(Egress::Denied)
        .expect("the view is probed again")
        .expect("the network moved");

    assert!(
        rig.last_argv().contains("--unshare-net"),
        "a narrowed session still runs with the host's network: {}",
        rig.last_argv()
    );
    // And the record the operator is shown is the boundary that runs.
    assert!(!host.boundary().expect("a boundary").egress.allowed());
    let _ = std::fs::remove_dir_all(&rig.dir);
}

/// A host with no view of its own has no network of ours to decide, and says so rather than
/// spending a probe.
#[test]
fn a_host_with_no_view_answers_nothing() {
    let backend = HostBackend::writable(std::env::temp_dir()).expect("a plain backend");
    assert_eq!(
        backend.reseat_egress(Egress::Denied).expect("no view"),
        None
    );
}
