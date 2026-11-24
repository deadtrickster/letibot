//! `[queue] gate_in = "firecode"`: the gate's steps run inside a firecode VM over the gate's
//! checkout, mounted read-only with nothing copied back. Deadlines, logs and verdicts are the
//! ordinary gate's. A machine without a working firecode leaves the tip UNJUDGED; the steps are
//! never run on the host instead.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use letibot_tools::rpc::{Ended, run_bounded};

use letibot_tools::exec::memory::Ask;

use super::gate::{GateControl, GateRun, GateRunner};

const DOCTOR_BOUND: Duration = Duration::from_secs(120);

/// Whether this machine can run a VM, and which firecode to run it with.
pub trait Ready: Send + Sync {
    fn ready(&self) -> Result<PathBuf, String>;
}

/// The real answer: the CLI is found, `/dev/kvm` is there on Linux, and `firecode doctor` passes.
pub struct Doctor;

impl Ready for Doctor {
    fn ready(&self) -> Result<PathBuf, String> {
        let bin = letibot_tools::firecode::firecode_available()?;
        if cfg!(target_os = "linux") && std::fs::metadata("/dev/kvm").is_err() {
            return Err("there is no /dev/kvm on this machine".into());
        }
        match run_bounded(&bin, &["doctor".to_string()], DOCTOR_BOUND) {
            Ended::Answered(o) if o.status.success() => Ok(bin),
            Ended::Answered(o) => Err(format!(
                "`firecode doctor` failed: {}",
                String::from_utf8_lossy(&[o.stdout, o.stderr].concat())
                    .trim()
                    .lines()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" / ")
            )),
            Ended::TimedOut => Err("`firecode doctor` did not answer in time".into()),
            Ended::Unstartable(e) => Err(format!("`firecode doctor` could not start: {e}")),
        }
    }
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The line that runs `step` in a VM over `dir`. A Mac mounts `dir` read-only with a throwaway
/// layer (`firecode sh`). Elsewhere `firecode run` sends the files and copies nothing back.
pub fn launch_line(bin: &str, step: &str, dir: &str) -> String {
    launch_line_subject(bin, step, dir, &super::gate::GateSubject::default())
}

/// [`launch_line`] for a run that carries what it is compared against.
pub fn launch_line_subject(
    bin: &str,
    step: &str,
    dir: &str,
    subject: &super::gate::GateSubject,
) -> String {
    let bin = sq(bin);
    launch_with_subject(&bin, step, dir, subject)
}

/// The program on a declared host: `$FIRECODE_BIN` there, else `firecode` on its `PATH`.
pub const REMOTE_BIN: &str = "\"${FIRECODE_BIN:-firecode}\"";

/// Where the VM keeps the changed-path list [`launch_with_subject`] writes: this process's
/// environment does not cross into the VM, so the list is made there.
const VM_CHANGED: &str = "/tmp/letibot-gate-changed";

/// [`launch_line`] with `bin` already shell-ready.
///
/// The `run` arm carries item 1's stamp: the gate's own owner (`letibot:gate-<key>:<pid>`), the
/// project the work is in, and `--purpose ephemeral` - a gate's VM has nothing to keep and is
/// discarded the moment its verdict is in.
pub fn launch_with(bin: &str, step: &str, dir: &str) -> String {
    launch_with_subject(bin, step, dir, &super::gate::GateSubject::default())
}

/// [`launch_with`] for a run that carries what it is compared against. The two facts go in front of
/// the step, inside the VM, because the daemon's environment does not reach it.
pub fn launch_with_subject(
    bin: &str,
    step: &str,
    dir: &str,
    subject: &super::gate::GateSubject,
) -> String {
    let stamp = letibot_tools::firecode::cli::Stamp {
        session: format!(
            "gate-{}",
            letibot_tools::firecode::cli::project_key(Path::new(dir))
        ),
        pid: std::process::id(),
        project: Some(letibot_tools::firecode::cli::project_key(Path::new(dir))),
        purpose: letibot_tools::firecode::cli::Purpose::Ephemeral,
        reuse: false,
    };
    let step = match subject.base.is_empty() && subject.changed.is_empty() {
        true => step.to_string(),
        false => format!("{}{step}", subject.prelude(VM_CHANGED)),
    };
    let step = sq(&step);
    format!(
        "if [ \"$(uname -s)\" = Darwin ]; then exec {bin} sh -c {step} {dir}; \
         else cd {dir} && exec {bin} run {stamp} --project {dir} -- bash -c {step}; fi",
        bin = bin,
        step = step,
        dir = sq(dir),
        stamp = stamp.flags().join(" "),
    )
}

/// The sentence a gate that cannot start a VM ends with.
pub fn unjudged(why: &str) -> String {
    format!(
        "`[queue] gate_in = \"firecode\"` asks for the steps to run in a firecode VM, and this \
         machine cannot start one: {why}. Nothing was run, and it was not run on the host \
         instead - the tip is UNJUDGED, not red, and is gated again on a later pass. Fix firecode \
         here (`firecode doctor`) or remove the key."
    )
}

/// The gate for a project that said `gate_in = "firecode"`.
pub struct FirecodeGate {
    pub host: Option<String>,
    pub ready: Arc<dyn Ready>,
    pub memory: Ask,
}

impl FirecodeGate {
    fn here(
        &self,
        worktree: &Path,
        steps: &[String],
        ctl: &GateControl,
        subject: &super::gate::GateSubject,
    ) -> GateRun {
        let bin = match self.ready.ready() {
            Ok(b) => b,
            Err(why) => return super::gate::could_not_run(steps, unjudged(&why)),
        };
        let dir = worktree.display().to_string();
        let bin_s = bin.display().to_string();
        let run = super::gate::run_with_launcher_subject(
            worktree,
            steps,
            ctl,
            self.memory,
            &|s| launch_line_subject(&bin_s, s, &dir, subject),
            subject,
        );
        // `run` leaves its VM up for the next call; the gate has no next call.
        if cfg!(not(target_os = "macos")) {
            let _ = run_bounded(
                &bin,
                &["down".into(), "--discard".into(), "--project".into(), dir],
                Duration::from_secs(60),
            );
        }
        run
    }

    fn on(
        &self,
        worktree: &Path,
        steps: &[String],
        ctl: &GateControl,
        subject: &super::gate::GateSubject,
    ) -> GateRun {
        match &self.host {
            Some(name) => match super::gate_host::firecode_ready(name, worktree) {
                Ok(()) => {
                    super::gate_host::run_on_host_for(name, worktree, steps, ctl, true, subject)
                }
                // The VM is the same Linux on either machine, so a host without firecode hands
                // the steps to this machine's VM rather than leaving the tip unjudged.
                Err(why) => {
                    eprintln!(
                        "  merge queue: {name} cannot start a firecode VM ({why}); the steps run \
                         in this machine's"
                    );
                    self.here(worktree, steps, ctl, subject)
                }
            },
            None => self.here(worktree, steps, ctl, subject),
        }
    }
}

impl GateRunner for FirecodeGate {
    fn run(&self, worktree: &Path, steps: &[String], ctl: &GateControl) -> GateRun {
        self.on(worktree, steps, ctl, &super::gate::GateSubject::default())
    }

    fn run_for(
        &self,
        worktree: &Path,
        steps: &[String],
        ctl: &GateControl,
        subject: &super::gate::GateSubject,
    ) -> GateRun {
        self.on(worktree, steps, ctl, subject)
    }

    fn name(&self) -> String {
        match &self.host {
            Some(h) => format!("{h} (firecode)"),
            None => "local (firecode)".to_string(),
        }
    }

    /// The same disk answer as the gate it wraps: this form builds in the same tree, so a floor a
    /// plain gate would refuse is not one a VM gets round.
    fn gate_policy(&self) -> super::gate_cache::Policy {
        super::gate_cache::policy()
    }
}

/// The runner a gate uses for a repository: the ordinary one, or its firecode form. A runner with
/// no firecode form (a test's) is refused, never run plainly.
pub fn runner_for(
    gate_in: super::settings::GateIn,
    ordinary: &Arc<dyn GateRunner>,
) -> Arc<dyn GateRunner> {
    use super::settings::GateIn;
    match gate_in {
        // The seat's default is resolved BEFORE a runner is asked for (`RepoQueue::start_gate`),
        // and an unresolved one is this machine rather than a surprise.
        GateIn::Host | GateIn::Seat => ordinary.clone(),
        GateIn::Firecode => ordinary
            .firecode()
            .unwrap_or_else(|| Arc::new(Refuse(ordinary.name()))),
    }
}

struct Refuse(String);

impl GateRunner for Refuse {
    fn run(&self, _: &Path, steps: &[String], _: &GateControl) -> GateRun {
        super::gate::could_not_run(
            steps,
            unjudged(&format!("the `{}` gate cannot place steps in a VM", self.0)),
        )
    }
}

#[cfg(test)]
mod tests;
