use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::*;
use crate::mergequeue::gate::{GateVerdict, LocalGate};
use crate::mergequeue::settings::{self, GateIn};
use letibot_tokencore::store::MergeGateOutcome;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "letibot-gate-fc-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("wt")).unwrap();
        Fixture { dir }
    }

    fn wt(&self) -> PathBuf {
        self.dir.join("wt")
    }

    fn record(&self) -> PathBuf {
        self.dir.join("record")
    }

    /// A `firecode` that records how it was called and runs the step where a VM would.
    fn fake_firecode(&self) -> PathBuf {
        let p = self.dir.join("firecode");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1\" in\n\
                 sh) cd \"$4\" && exec sh -c \"$3\" ;;\n\
                 run) cd \"$3\" && exec sh -c \"$7\" ;;\n\
                 *) exit 0 ;;\nesac\n",
                self.record().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Fake(Result<PathBuf, String>);

impl Ready for Fake {
    fn ready(&self) -> Result<PathBuf, String> {
        self.0.clone()
    }
}

fn gate(ready: Result<PathBuf, String>) -> FirecodeGate {
    FirecodeGate {
        host: None,
        ready: Arc::new(Fake(ready)),
        memory: Ask::Uncapped,
    }
}

fn run(g: &FirecodeGate, wt: &Path, steps: &[&str], secs: u64) -> GateRun {
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(secs),
        budget: Duration::from_secs(secs),
        stop: &stop,
        alive: &alive,
    };
    let steps: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
    g.run(wt, &steps, &ctl)
}

#[test]
fn steps_run_through_firecode_with_the_gates_own_verdicts() {
    let f = Fixture::new("through");
    let g = gate(Ok(f.fake_firecode()));
    let r = run(&g, &f.wt(), &["echo one", "echo two"], 60);
    assert_eq!(r.verdict, GateVerdict::Passed, "{r:?}");
    let rec = std::fs::read_to_string(f.record()).unwrap();
    // Linux also brings its VM down once the gate has run.
    let calls = rec.lines().filter(|l| !l.starts_with("down ")).count();
    assert_eq!(calls, 2, "{rec}");
    assert!(
        rec.contains("echo one") && rec.contains("echo two"),
        "{rec}"
    );
    assert!(rec.contains(&f.wt().display().to_string()), "{rec}");
    assert!(
        r.steps
            .iter()
            .all(|s| s.outcome == MergeGateOutcome::Passed)
    );
    assert!(r.steps[0].output.contains("one"), "{:?}", r.steps[0]);
}

#[test]
fn a_red_step_is_red_and_stops_the_run() {
    let f = Fixture::new("red");
    let g = gate(Ok(f.fake_firecode()));
    let r = run(&g, &f.wt(), &["echo bad; exit 3", "echo never"], 60);
    assert!(
        matches!(r.verdict, GateVerdict::Failed(_)),
        "{:?}",
        r.verdict
    );
    assert_eq!(r.steps[1].outcome, MergeGateOutcome::NotRun);
    let rec = std::fs::read_to_string(f.record()).unwrap();
    assert!(!rec.contains("never"), "{rec}");
}

#[test]
fn the_gates_deadline_still_applies() {
    let f = Fixture::new("deadline");
    let g = gate(Ok(f.fake_firecode()));
    let r = run(&g, &f.wt(), &["sleep 30"], 1);
    assert!(
        matches!(r.verdict, GateVerdict::TimedOut(_)),
        "{:?}",
        r.verdict
    );
}

/// No working firecode is a gate that judged nothing, and the step never ran on the host.
#[test]
fn no_firecode_leaves_the_tip_unjudged_and_runs_nothing_on_the_host() {
    let f = Fixture::new("none");
    let marker = f.dir.join("ran-on-the-host");
    for ready in [
        Err("`firecode doctor` failed: no /dev/kvm".to_string()),
        Err("there is no `firecode` on this host's PATH".to_string()),
    ] {
        let g = gate(ready);
        let step = format!("touch '{}'", marker.display());
        let r = run(&g, &f.wt(), &[step.as_str()], 60);
        let GateVerdict::Infra(why) = &r.verdict else {
            panic!("{:?}", r.verdict);
        };
        assert!(
            why.contains("UNJUDGED") && why.contains("not run on the host"),
            "{why}"
        );
        assert!(why.contains("gate_in"), "{why}");
        assert_eq!(r.steps[0].outcome, MergeGateOutcome::NotRun);
        assert!(!marker.exists(), "the step ran on the host");
    }
}

#[test]
fn the_default_is_the_ordinary_runner_and_firecode_is_opt_in() {
    let ordinary: Arc<dyn GateRunner> = Arc::new(LocalGate);
    assert_eq!(runner_for(GateIn::Host, &ordinary).name(), "local");
    assert_eq!(
        runner_for(GateIn::Firecode, &ordinary).name(),
        "local (firecode)"
    );
}

/// A runner with no firecode form is refused for a firecode project, never run plainly.
#[test]
fn a_runner_without_a_firecode_form_is_refused() {
    struct Plain;
    impl GateRunner for Plain {
        fn run(&self, _: &Path, steps: &[String], _: &GateControl) -> GateRun {
            panic!("ran plainly: {steps:?}")
        }
    }
    let f = Fixture::new("plain");
    let ordinary: Arc<dyn GateRunner> = Arc::new(Plain);
    let r = runner_for(GateIn::Firecode, &ordinary);
    let stop = AtomicBool::new(false);
    let alive = || true;
    let ctl = GateControl {
        deadline: Instant::now() + Duration::from_secs(5),
        budget: Duration::from_secs(5),
        stop: &stop,
        alive: &alive,
    };
    let out = r.run(&f.wt(), &["true".to_string()], &ctl);
    assert!(matches!(out.verdict, GateVerdict::Infra(_)));
}

#[test]
fn gate_in_is_read_from_the_queue_table() {
    let f = Fixture::new("settings");
    let p = f.dir.join("leticode.toml");
    assert_eq!(
        settings::parse(&p.with_file_name("none.toml")).is_err(),
        true
    );
    std::fs::write(&p, "[queue]\n").unwrap();
    assert_eq!(settings::parse(&p).unwrap().gate_in, GateIn::Host);
    std::fs::write(&p, "[queue]\ngate_in = \"firecode\"\n").unwrap();
    assert_eq!(settings::parse(&p).unwrap().gate_in, GateIn::Firecode);
    std::fs::write(&p, "[queue]\ngate_in = \"vm\"\n").unwrap();
    let e = settings::parse(&p).unwrap_err();
    assert!(e.contains("gate_in") && e.contains("firecode"), "{e}");
}

#[test]
fn the_launch_line_mounts_on_a_mac_and_runs_elsewhere() {
    let l = launch_line("/b/firecode", "cargo test", "/w t");
    assert!(
        l.contains("exec '/b/firecode' sh -c 'cargo test' '/w t'"),
        "{l}"
    );
    assert!(
        l.contains("run --project '/w t' -- bash -c 'cargo test'"),
        "{l}"
    );
}

/// The real thing, over a throwaway directory. Run with `--ignored` where `firecode doctor` passes.
#[test]
#[ignore = "needs a working firecode (firecode doctor) and runs a real VM"]
fn a_real_firecode_runs_a_step_over_the_checkout() {
    let f = Fixture::new("real");
    std::fs::write(f.wt().join("marker.txt"), "from the host").unwrap();
    let g = FirecodeGate {
        host: None,
        ready: Arc::new(Doctor),
        memory: Ask::Uncapped,
    };
    let r = run(&g, &f.wt(), &["cat marker.txt && uname -s"], 600);
    assert_eq!(r.verdict, GateVerdict::Passed, "{r:?}");
    assert!(
        r.steps[0].output.contains("from the host"),
        "{:?}",
        r.steps[0]
    );
}
