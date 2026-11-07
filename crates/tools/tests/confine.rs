//! Acceptance tests for the namespace half of layer 1: the measured boundary, the refusals when
//! there is none, and the two properties the mount view exists for. Each asserts both branches, as a
//! skip would be green having measured nothing.

use std::path::{Path, PathBuf};

use letibot_tools::exec::confine::{
    BWRAP_ENV, Boundary, ConfinePlan, Confinement, Egress, Grant, HomeView, Namespace,
    NoConfinement, NsState, Seal, SealKind, TmpView, Unconfined, ViewSpec,
};
use letibot_tools::exec::{Bwrap, ExecError, HostProcesses, ProcessHost, ScopeKind, SpawnRequest};
use letibot_tools::testing::{confined_harness, confined_harness_with};
use letibot_transcript::ToolOutcome;

/// The harness, or the refusal branch: a box with no boundary still asserts on the
/// refusal instead of skipping.
macro_rules! confined {
    ($name:literal) => {
        match confined_harness() {
            Ok(h) => h,
            Err(e) => {
                no_boundary(&e, $name);
                return;
            }
        }
    };
}

/// What a skip in this file names, for [`letibot_tokencore::apparatus::absent`].
const CGROUP: &str = "a cgroup v2 subtree this user may write (the process-lifetime tree)";
const BOUNDARY: &str =
    "a usable confinement boundary (bwrap, an unprivileged user namespace, a cgroup v2 subtree)";

/// The one test in this file that sets a PROCESS-WIDE variable, and libtest runs this file's tests
/// on threads of one process: without a guard a sibling could spawn while the token is set, which is
/// the shape `std::env::set_var` is `unsafe` for.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A skip the require-switch can refuse: through `apparatus::absent` the skip is loud
/// without `LETIBOT_REQUIRE_APPARATUS` and a failure with it.
fn absent(what: &str) {
    letibot_tokencore::apparatus::absent(what)
}

/// The boundary is a platform question before it is a box question: on Linux a missing one
/// is apparatus this box was not given, while macOS can never have one at all, so the
/// refusal asserted just above is the whole of what a boundary test can measure there.
fn boundary_absent() {
    let Some(_) = letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux")) else {
        return;
    };
    absent(BOUNDARY);
}

/// The branch where this box cannot confine: the refusal must name what was missing and say
/// nothing ran, because a boundary that is silently not there is worse than none. It is
/// asserted before the absence is announced, so a run that demands the apparatus fails.
fn no_boundary(e: &ExecError, test: &str) {
    eprintln!("{test}: no usable boundary on this host, asserting on the refusal instead: {e}");
    let m = format!("{e}");
    assert!(
        m.contains("was NOT run") || m.contains("nothing would reap it"),
        "a refusal must say that nothing ran: {m}"
    );
    assert!(
        m.contains("bwrap")
            || m.contains("namespace")
            || m.contains("cgroup")
            || m.contains("userns"),
        "a refusal must name the mechanism that was missing: {m}"
    );
    boundary_absent();
}

// --------------------------------------- the require switch reaches every skip

/// A missing boundary is a SKIP, and a run that demands the apparatus fails instead - pinned by
/// running this binary once more with the helper pointed at nothing and the switch set, because no
/// run can set either variable under its own tests.
#[test]
fn a_missing_boundary_fails_the_run_that_demands_the_apparatus() {
    let Some(_) = letibot_tokencore::apparatus::applies(BOUNDARY, cfg!(target_os = "linux")) else {
        return;
    };
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "describe_reports_the_boundary_that_was_measured_and_not_the_one_requested",
            "--nocapture",
        ])
        .env(BWRAP_ENV, "/nonexistent/bwrap")
        .env("LETIBOT_REQUIRE_APPARATUS", "1")
        .env("RUST_TEST_THREADS", "1")
        .output()
        .expect("the test binary runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a boundary that is not there must be refused by the switch, not skipped: {said}"
    );
    assert!(said.contains("LETIBOT_REQUIRE_APPARATUS is set"), "{said}");
    assert!(said.contains("1 failed"), "{said}");
}

// ------------------------------------------------------- rule 1: fail closed

#[test]
fn a_session_that_asks_for_a_boundary_and_has_none_refuses_and_never_runs_unconfined() {
    // The rule that outranks the rest, tested for real rather than on this kernel's happy
    // path: the helper is pointed at a path that is not there, which is what a host without
    // bubblewrap looks like from inside the code.
    let e = Bwrap::probe(ViewSpec::project_only("/"), Egress::Denied);
    // Guard the fact, not the proxy: point the lookup at nothing and re-probe.
    // SAFETY: this test does not spawn threads before restoring the variable.
    unsafe { std::env::set_var(BWRAP_ENV, "/nonexistent/bwrap") };
    let refused = Bwrap::probe(ViewSpec::project_only("/tmp"), Egress::Denied);
    unsafe { std::env::remove_var(BWRAP_ENV) };
    drop(e);

    let err = refused.expect_err("a helper that is not there cannot confine");
    let m = format!("{err}");
    // On Linux the refusal names the helper it could not find; on macOS no helper is looked
    // for, because there are no namespaces for one to enter, and it says that instead.
    if cfg!(target_os = "macos") {
        assert!(m.contains("macOS has no namespace boundary"), "{m}");
    } else {
        assert!(m.contains("/nonexistent/bwrap"), "{m}");
    }
    assert!(m.contains("was NOT run"), "{m}");
    assert!(
        m.contains("Running it unconfined is not an available outcome"),
        "the refusal must close the door it is standing in: {m}"
    );
}

#[test]
fn a_host_with_no_confinement_refuses_every_spawn_and_starts_nothing() {
    // The full substrate, with a real cgroup tree and a boundary that is missing.
    // The cgroup half is what makes this a test rather than a unit assertion: the
    // spawn gets far enough to have opened a scope, and must leave none behind.
    let h = match confined_harness_with(|_root| {
        Err(ExecError::NoConfinement(
            "this test deliberately supplies no boundary".into(),
        ))
    }) {
        Ok(_) => panic!("a confinement that refuses to build must not yield a harness"),
        Err(e) => e,
    };
    let m = format!("{h}");
    assert!(m.contains("deliberately supplies no boundary"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");

    // And through the substrate: a host whose confinement refuses starts nothing,
    // and the scope it opened on the way is taken back.
    let Ok(host) = HostProcesses::confined(
        std::env::temp_dir(),
        Box::new(NoConfinement::new("no boundary on this host")),
    ) else {
        // The confinement refusal is still asserted above.
        return absent(CGROUP);
    };
    let before = host.scopes().len();
    let e = host
        .spawn(&SpawnRequest {
            command: "echo this must not run".into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect_err("a host with no boundary must not spawn");
    let m = format!("{e}");
    assert!(m.contains("no boundary on this host"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
    assert!(host.jobs().is_empty(), "nothing may be recorded as running");
    // Only the refused job's scope must go: the session and turn scopes are opened on the
    // way and are not the job's, which is why this counts job scopes rather than scopes.
    let _ = before;
    let job_scopes: Vec<_> = host
        .scopes()
        .into_iter()
        .filter(|s| s.name.starts_with("job-"))
        .collect();
    assert!(
        job_scopes.is_empty(),
        "a refused spawn must not leave a job scope behind: {job_scopes:?}"
    );
}

#[test]
fn a_bash_call_on_a_refusing_boundary_says_nothing_ran_and_is_not_a_denial() {
    let Ok(host) = HostProcesses::confined(
        std::env::temp_dir(),
        Box::new(NoConfinement::new(
            "`bwrap` is not on `PATH` in this fixture",
        )),
    ) else {
        return absent(CGROUP);
    };
    // The path a tool actually takes: `spawn` fails, and `bash` renders it.
    let e = host
        .spawn(&SpawnRequest {
            command: "true".into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .unwrap_err();
    let m = format!("{e}");
    // The two things a model has to be able to act on.
    assert!(m.contains("bwrap"), "which mechanism: {m}");
    assert!(m.contains("was NOT run"), "what happened: {m}");
    // And it must not read as a policy denial: nobody decided anything about this
    // command, the substrate is simply not there.
    assert!(!m.to_lowercase().contains("denied"), "{m}");
}

// -------------------------------------- rule 2: describe what you actually got

#[test]
fn describe_reports_the_boundary_that_was_measured_and_not_the_one_requested() {
    let h = confined!("describe_reports_measured");
    let host = h.processes.as_ref().expect("a confined harness has a host");
    let b = host.boundary().expect("a measured boundary");

    // Every namespace's state carries the INODE read back from inside, which is
    // the evidence. A hard-coded `confined` has no inode to show.
    for n in Namespace::REQUIRED {
        match b.ns.get(&n) {
            Some(NsState::Entered { inode }) => {
                assert!(inode.starts_with(n.proc_name()), "{n}: {inode}");
                assert!(inode.contains('['), "an inode, not a word: {inode}");
            }
            other => panic!("{n} must be entered on a confined host, got {other:?}"),
        }
    }
    assert!(b.complete(), "{}", b.describe());

    // And the inode is not the harness's own, which is the comparison that makes the
    // readback a measurement rather than a formality.
    for n in Namespace::REQUIRED {
        let mine = std::fs::read_link(format!("/proc/self/ns/{}", n.proc_name()))
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if let Some(NsState::Entered { inode }) = b.ns.get(&n) {
            assert_ne!(
                inode,
                &mine.as_str(),
                "{n} inside must differ from the harness's"
            );
        }
    }

    // The disclosure names the mechanism with its version, not a category.
    let d = host.confinement_describe();
    assert!(d.contains("bubblewrap"), "{d}");
    assert!(d.contains("entered ("), "{d}");
}

#[test]
fn a_partial_boundary_describes_itself_as_partial_through_the_whole_stack() {
    // Driven from the top rather than from a fixture struct: what a caller reads has to
    // become partial when the state is. The `Confinement` below reports a boundary with one
    // non-required namespace missing, and nothing above it may hold a constant.
    struct PartiallyConfined(Boundary);
    impl Confinement for PartiallyConfined {
        fn describe(&self) -> String {
            self.0.describe()
        }
        fn boundary(&self) -> Option<&Boundary> {
            Some(&self.0)
        }
        fn wrap(&self, _p: &ConfinePlan<'_>) -> Result<Vec<String>, ExecError> {
            Ok(Vec::new())
        }
    }

    // Built from a real measurement where possible, so the partial case is a real boundary
    // minus one namespace rather than an invention.
    let mut b = match Bwrap::project(std::env::temp_dir()) {
        Ok(real) => real.boundary().expect("measured").clone(),
        Err(e) => {
            no_boundary(&e, "a_partial_boundary_describes_itself");
            return;
        }
    };
    b.ns.insert(
        Namespace::Ipc,
        NsState::NotEntered {
            why: "the inode inside was the harness's own".into(),
        },
    );

    let Ok(host) = HostProcesses::confined(std::env::temp_dir(), Box::new(PartiallyConfined(b)))
    else {
        return absent(CGROUP);
    };
    let d = ProcessHost::describe(&host);
    assert!(d.contains("PARTIALLY"), "the host must say partial: {d}");
    assert!(d.contains("NOT ENTERED"), "and which one: {d}");
    assert!(d.contains("ipc"), "{d}");
    // The required ones are still there, so it does still confine: the disclosure has to say
    // both things at once rather than collapse to a boolean.
    assert!(!d.contains("NOT CONFINED"), "{d}");
}

#[test]
fn the_backend_disclosure_no_longer_hard_codes_a_confinement_claim() {
    // Two different confinements must produce two different disclosures: a constant
    // asserting a session property passes a one-armed test.
    use letibot_tools::backend::{ExecBackend, HostBackend};
    let dir = std::env::temp_dir().join(format!("letibot-describe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");

    // The unconfined exec backend: it must say so, in the disclosure the daemon
    // prints at startup.
    match HostBackend::executable(&dir) {
        Ok(b) => {
            let d = b.describe();
            assert!(d.contains("EXEC"), "{d}");
            assert!(d.contains("NOT CONFINED"), "{d}");
            assert!(d.contains("whole filesystem"), "{d}");
        }
        Err(e) => {
            eprintln!("{e}");
            absent(CGROUP);
        }
    }

    // The confined one: the same function and a different sentence, because the sentence is
    // read off the state rather than written into the match arm.
    match HostBackend::confined(&dir) {
        Ok(b) => {
            let d = b.describe();
            assert!(d.contains("EXEC"), "{d}");
            assert!(
                !d.contains("NOT CONFINED"),
                "a confined backend must not describe itself as unconfined: {d}"
            );
            assert!(d.contains("confined"), "{d}");
            // Which mechanism is a platform fact: Linux enters namespaces through `bwrap`,
            // macOS applies a Seatbelt profile through `sandbox-exec`. The disclosure must
            // name the thing that did it, so either answer passes and neither is a hole.
            assert!(
                d.contains("bubblewrap") || d.contains("sandbox-exec"),
                "the mechanism, named: {d}"
            );
        }
        Err(e) => {
            let m = format!("{e}");
            eprintln!("no confined backend on this host: {m}");
            assert!(
                m.contains("was NOT run") || m.contains("nothing would reap it"),
                "the refusal must say nothing ran: {m}"
            );
            boundary_absent();
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unconfined_is_a_declared_state_and_not_an_absent_one() {
    // The third of the three states, and it must be distinguishable from "asked for and
    // missing": the difference has to reach a reader.
    let Ok(host) = HostProcesses::new(std::env::temp_dir()) else {
        return absent(CGROUP);
    };
    let d = ProcessHost::describe(&host);
    assert!(d.contains("NOT CONFINED"), "{d}");
    assert!(d.contains("whole filesystem"), "{d}");
    // And it is not the same sentence as a refusal: this one runs commands.
    assert!(!d.contains("refuses to exec"), "{d}");

    let refusing = Unconfined::because("x");
    assert!(
        refusing
            .wrap(&ConfinePlan {
                cwd: Path::new("/"),
                env: &[]
            })
            .is_ok()
    );
    assert!(
        NoConfinement::new("y")
            .wrap(&ConfinePlan {
                cwd: Path::new("/"),
                env: &[]
            })
            .is_err()
    );
}

// ------------------------------- rule 3: the mount view is the enforcement point

#[test]
fn a_secret_outside_the_project_is_absent_rather_than_denied() {
    // The point of the entry: not "the read was refused", because there is nothing there
    // to refuse. The secret is a fixture, because naming the operator's own ssh directory in
    // a command is refused before the boundary is reached and the boundary would go unmeasured.
    let mut h = confined!("a_secret_outside_the_project_is_absent");
    let outside = std::env::temp_dir().join(format!("letibot-secret-{}", std::process::id()));
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::write(
        outside.join("credential.pem"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n",
    )
    .expect("fixture secret");

    // Two calls, because there are two mechanisms and one would hide the other: the flow
    // rule refuses a credential-shaped read before the boundary is reached, so the boundary is
    // measured on a plain file and the flow rule separately below.
    std::fs::write(outside.join("notes.txt"), "outside contents\n").expect("fixture note");
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": format!(
                "ls -a {d} 2>&1; cat {d}/notes.txt 2>&1; true",
                d = outside.display()
            ),
        })
        .to_string(),
    );
    let text = payload(&r);

    // The second mechanism, in front of the first: naming the credential is refused by the
    // flow rule and nothing runs, though the boundary above already made the bytes
    // unreachable.
    let refused = h.call(
        "bash",
        &serde_json::json!({
            "command": format!("cat {d}/credential.pem", d = outside.display()),
        })
        .to_string(),
    );
    let refused_text = format!("{refused:?}");
    assert!(
        matches!(refused.outcome, ToolOutcome::Denied { .. }),
        "a `.pem` read into the transcript is a disclosure: {refused_text}"
    );
    assert!(
        !refused_text.contains("PRIVATE KEY"),
        "the refusal must not carry the bytes it refused: {refused_text}"
    );

    let _ = std::fs::remove_dir_all(&outside);

    // No key bytes by any spelling: the assertion is on the marker, not on the filename,
    // because the filename is the thing a path list gets wrong.
    assert!(
        !text.contains("PRIVATE KEY"),
        "a private key reached the transcript: {text}"
    );
    // And the mechanism is absence: the path is not there to be denied.
    assert!(
        text.contains("No such file or directory") || text.contains("cannot access"),
        "the secret must be ABSENT, not merely unreadable: {text}"
    );
    assert!(!text.contains("outside contents"), "not readable: {text}");
}

#[test]
fn the_project_itself_is_readable_and_writable_at_its_own_path() {
    // The half that makes the boundary usable rather than merely safe: the path is the same
    // inside and out, or every compiler diagnostic names a file the read tools cannot open.
    let mut h = confined!("the_project_is_readable_and_writable");
    let root = h.root().canonicalize().expect("fixture root");
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": format!("pwd; echo confined-write > {}/written.txt; cat {}/written.txt", root.display(), root.display()),
        })
        .to_string(),
    );
    let text = payload(&r);
    assert!(text.contains(&root.display().to_string()), "{text}");
    assert!(text.contains("confined-write"), "{text}");
    // Read from the real disk, not through the backend: a test that asked the tool whether
    // it wrote is a test of the tool's opinion of itself.
    assert_eq!(h.read_file("written.txt").trim(), "confined-write");
}

#[test]
fn a_sibling_project_is_not_in_the_view() {
    // The property the project-scoped part buys: one project, not the operator's whole
    // code tree.
    let mut h = confined!("a_sibling_project_is_not_in_the_view");
    let root = h.root().canonicalize().expect("root");
    let sibling = root
        .parent()
        .expect("a parent")
        .join(format!("letibot-sibling-{}", std::process::id()));
    std::fs::create_dir_all(&sibling).expect("sibling");
    std::fs::write(sibling.join("secret.txt"), "sibling contents").expect("sibling file");

    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": format!("cat {}/secret.txt 2>&1; true", sibling.display()),
        })
        .to_string(),
    );
    let text = payload(&r);
    let _ = std::fs::remove_dir_all(&sibling);
    assert!(!text.contains("sibling contents"), "{text}");
    assert!(text.contains("No such file"), "{text}");
}

// --------------------------------------------------- rule 3: PID and network

#[test]
fn the_command_cannot_see_or_signal_the_harness_or_the_servers_it_manages() {
    // `protect_listener` and `predicate` become belt-and-braces here: the pid is
    // not merely refused as an argument, it does not exist in this namespace.
    let mut h = confined!("cannot_see_or_signal");
    let me = std::process::id();
    let r = h.call(
        "bash",
        &serde_json::json!({
            // No `$(...)` and no `$?`: both are unresolvable until the shell runs, and an
            // unresolvable command is `not_run` before it reaches the boundary. `sed` and
            // `||` do the labelling they did.
            "command": format!(
                "ls /proc | grep -c '^[0-9]' | sed 's/^/pids=/'; kill -0 {me} 2>&1 || echo rc=1"
            ),
        })
        .to_string(),
    );
    let text = payload(&r);
    // A handful of pids, not the box's several hundred. The number is the
    // denominator: "a small number" is what makes this a measurement.
    let n: usize = text
        .split("pids=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    assert!(n < 20, "the sandbox saw {n} processes: {text}");
    // And the harness's own pid is not signalable, because it is not there.
    assert!(
        text.contains("No such process") || text.contains("rc=1"),
        "the harness pid must not be reachable: {text}"
    );
}

#[test]
fn egress_is_denied_by_default_and_the_disclosure_says_so() {
    // `web_fetch` is a seam only if an unconfined `curl` is not an alternative.
    let mut h = confined!("egress_is_denied_by_default");
    let host = h.processes.as_ref().expect("host").clone();
    let d = host.confinement_describe();
    assert!(d.contains("DENIED"), "{d}");
    assert_eq!(host.boundary().map(|b| b.egress), Some(Egress::Denied));

    // There are no interfaces at all, which is a stronger statement than "the
    // connect failed" and does not depend on anything being reachable from this
    // box in the first place.
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "ip -o link show 2>/dev/null | wc -l; cat /proc/net/dev | tail -n +3 | wc -l"})
            .to_string(),
    );
    let text = payload(&r);
    let lines: Vec<usize> = text
        .lines()
        .filter_map(|l| l.trim().parse::<usize>().ok())
        .collect();
    assert!(
        lines.iter().any(|n| *n <= 1),
        "an empty network namespace has at most `lo`: {text}"
    );
}

#[test]
fn a_declared_egress_is_a_decision_in_the_record_and_not_a_broken_boundary() {
    // The case that must not be reported as a defect: `Egress::Host` shares the host's network
    // namespace on purpose, so the record says decision rather than not entered, or an
    // authorised `git push` reads as a boundary failure.
    let view = ViewSpec::project_only(std::env::temp_dir());
    let b = match Bwrap::probe(
        view,
        Egress::Host {
            why: "the operator authorised `git push` to origin in this turn".into(),
        },
    ) {
        Ok(b) => b,
        Err(e) => {
            no_boundary(&e, "a_declared_egress_is_a_decision");
            return;
        }
    };
    let boundary = b.boundary().expect("measured");
    assert!(boundary.complete(), "{}", boundary.describe());
    assert!(
        !boundary.partial(),
        "a decision is not a missing namespace: {}",
        boundary.describe()
    );
    assert!(matches!(
        boundary.ns.get(&Namespace::Net),
        Some(NsState::SharedByDecision { .. })
    ));
    let d = b.describe();
    assert!(d.contains("git push"), "{d}");
    assert!(d.contains("SHARED WITH THE HOST by decision"), "{d}");
}

// ------------------------------- the seal: a child must not be able to step out

#[test]
fn a_child_cannot_unshare_its_way_back_out_of_the_boundary() {
    // `unshare(CLONE_NEWUSER)` is available to an unprivileged process and gives it full
    // capabilities in the namespace it just made, which is enough to unshare the mount view
    // its parent built. Mount, PID and network namespaces are decoration unless this is closed.
    let mut h = confined!("a_child_cannot_unshare_its_way_out");
    let host = h.processes.as_ref().expect("host").clone();
    let b = host.boundary().expect("measured");

    // First the measurement the substrate took at probe time: no seal may be open,
    // or the probe would have refused to hand back a boundary at all.
    match b.seals.get(&SealKind::NestedNamespaces) {
        Some(Seal::Held { .. }) => {}
        Some(Seal::Unverified { why }) => {
            eprintln!("the nested-namespace seal could not be tested here: {why}");
        }
        other => panic!("a boundary with an open seal must never be handed back: {other:?}"),
    }
    assert!(
        host.confinement_describe()
            .contains("seal nested-namespaces"),
        "the disclosure must state the seal: {}",
        host.confinement_describe()
    );

    // Then the escape itself, tried through the tool the model would use.
    let r = h.call(
        "bash",
        &serde_json::json!({
            "command": "unshare -U true 2>&1 && echo ESCAPED || echo sealed; \
                        unshare -Urm --map-root-user true 2>&1 && echo ESCAPED-MOUNT || echo sealed-mount"
        })
        .to_string(),
    );
    let text = payload(&r);
    assert!(
        !text.contains("ESCAPED"),
        "a process inside the boundary made itself a namespace: {text}"
    );
}

#[test]
fn no_new_privs_is_read_from_the_kernel_rather_than_assumed_from_the_flag() {
    // An unprivileged restrict is rejected without `PR_SET_NO_NEW_PRIVS` set first, which is
    // an ordering fact and not a style. This asserts the result read from the kernel, because
    // a helper's documented behaviour is only the helper's word.
    let h = confined!("no_new_privs_is_read");
    let host = h.processes.as_ref().expect("host").clone();
    let b = host.boundary().expect("measured");
    match b.seals.get(&SealKind::NoNewPrivs) {
        Some(Seal::Held { how }) => assert!(how.contains("NoNewPrivs: 1"), "{how}"),
        Some(Seal::Unverified { why }) => eprintln!("no-new-privs unverified here: {why}"),
        other => panic!("no-new-privs must not be open in a boundary handed back: {other:?}"),
    }
}

// --------------------------------------------- rule 4: absence must be legible

#[test]
fn a_path_outside_the_view_produces_a_boundary_note_and_not_a_bare_enoent() {
    // The empty-haystack case: `No such file or directory` is read by a model as an answer
    // about the world, and here it is an answer about the view. The path is a plain fixture
    // because a credential-shaped one is refused before the boundary is reached.
    let mut h = confined!("a_path_outside_the_view_produces_a_note");
    let outside = PathBuf::from(format!("/opt/letibot-note-{}", std::process::id()));
    let r = h.call(
        "bash",
        &serde_json::json!({"command": format!("cat {}/thing.txt 2>&1; true", outside.display())})
            .to_string(),
    );
    let all = format!("{:?}", r);
    assert!(
        all.contains("not in this session's filesystem view"),
        "the ENOENT must be explained as the boundary: {all}"
    );
    assert!(
        all.contains("no other spelling"),
        "and the note must close the retry loop: {all}"
    );
}

#[test]
fn a_genuinely_missing_project_file_is_not_blamed_on_the_boundary() {
    // The false positive that would cost the most: a model told to ask for a grant
    // it does not need, about a file it should simply create.
    let mut h = confined!("a_genuinely_missing_project_file");
    let root = h.root().canonicalize().expect("root");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": format!("cat {}/definitely-absent.txt 2>&1; true", root.display())})
            .to_string(),
    );
    let all = format!("{:?}", r);
    assert!(all.contains("No such file"), "{all}");
    assert!(
        !all.contains("not in this session's filesystem view"),
        "a miss INSIDE the view is not a boundary note: {all}"
    );
}

// --------------------------------------------------- the credential decision

#[test]
fn there_is_no_grant_that_puts_a_private_key_in_the_view() {
    // `ssh` legitimately reads a private key, so the key is never made readable: an agent
    // socket is forwarded instead, usable without being readable into context. Asserted on
    // the API surface, because the decision is a property of what can be spelled.
    let dir = std::env::temp_dir().join(format!("letibot-cred-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let key = dir.join("id_rsa");
    std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\n").expect("key");

    // SAFETY: no threads are spawned in this test between set and remove.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &key) };
    let e = Grant::agent_from_env("the operator said `ssh build-box`").unwrap_err();
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
    let m = format!("{e}");
    assert!(m.contains("not a socket"), "{m}");
    assert!(
        m.contains("usable without being readable"),
        "the refusal must state the property it is protecting: {m}"
    );
    assert!(m.contains("Nothing was bound"), "{m}");

    // A `ReadOnly` grant of the same key is spellable, so it must announce the consequence:
    // this is the one path by which somebody could make a key readable, and it cannot be
    // taken quietly.
    let v = ViewSpec::project_only("/proj").granting(Grant::ReadOnly {
        path: key.clone(),
        why: "somebody insisted".into(),
    });
    assert!(
        v.summary().contains("READABLE INTO CONTEXT"),
        "{}",
        v.summary()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_forwarded_agent_socket_is_in_the_view_at_a_fixed_path_and_the_key_is_not() {
    // What the mechanism actually does, run: the socket appears, `SSH_AUTH_SOCK`
    // points at it, and the directory the real key lives in is still absent.
    let dir = std::env::temp_dir().join(format!("letibot-agent-run-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let keydir = dir.join("keys");
    std::fs::create_dir_all(&keydir).expect("keydir");
    std::fs::write(
        keydir.join("id_rsa"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\n",
    )
    .expect("key");
    let sock = dir.join("agent.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("agent socket");

    let project = dir.join("project");
    std::fs::create_dir_all(&project).expect("project");
    let view = ViewSpec::project_only(&project).granting(Grant::AgentSocket {
        path: sock.clone(),
        why: "the operator said `ssh build-box`".into(),
    });
    let b = match Bwrap::probe(view, Egress::Denied) {
        Ok(b) => b,
        Err(e) => {
            no_boundary(&e, "a_forwarded_agent_socket");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    };
    let Ok(host) = HostProcesses::confined(&project, Box::new(b)) else {
        let _ = std::fs::remove_dir_all(&dir);
        return absent(CGROUP);
    };
    let id = host
        .spawn(&SpawnRequest {
            command: format!(
                "echo sock=$SSH_AUTH_SOCK; test -S \"$SSH_AUTH_SOCK\" && echo is-a-socket; \
                 cat {}/id_rsa 2>&1; true",
                keydir.display()
            ),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("spawn");
    let _ = host.wait_job(&id, std::time::Duration::from_secs(20));
    let text = host
        .output(&id, 0, usize::MAX)
        .map(|o| o.text())
        .unwrap_or_default();
    drop(listener);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        text.contains("is-a-socket"),
        "the agent must be usable: {text}"
    );
    assert!(
        text.contains("/run/letibot/agent.sock"),
        "at a fixed path, so the operator's own socket path does not travel: {text}"
    );
    assert!(
        !text.contains("PRIVATE KEY"),
        "the key must stay outside the view even when its agent is forwarded: {text}"
    );
}

// ------------------------------------------------- the two halves compose

#[test]
fn the_cgroup_still_owns_a_confined_process_and_reaps_it() {
    // The join happens on the HOST, before the namespaces exist, since `/sys/fs/cgroup` is deliberately
    // not in the view: a confined process is a member of its scope and `cgroup.kill` reaches it across the
    // pid namespace.
    let mut h = confined!("the_cgroup_still_owns_a_confined_process");
    let r = h.call(
        "bash",
        &serde_json::json!({"command": "sleep 30", "background": true}).to_string(),
    );
    let text = payload(&r);
    let host = h.processes.as_ref().expect("host").clone();
    let jobs = host.jobs();
    assert_eq!(jobs.len(), 1, "{text}");
    let job = &jobs[0];
    let members = cgroup_members(&job.scope);
    assert!(
        members.contains(&job.pid),
        "the pid the harness tracks must be in the cgroup: {members:?} vs {}",
        job.pid
    );
    let reaping = host.kill_job(&job.id).expect("kill");
    assert!(
        !reaping.observed.is_empty(),
        "the reap must have seen the confined process: {}",
        reaping.summary()
    );
    assert!(reaping.survivors.is_empty(), "{}", reaping.summary());
}

#[test]
fn a_cwd_outside_the_view_refuses_before_anything_is_started() {
    let b = match Bwrap::project(std::env::temp_dir()) {
        Ok(b) => b,
        Err(e) => return no_boundary(&e, "a_cwd_outside_the_view_refuses"),
    };
    let e = b
        .wrap(&ConfinePlan {
            cwd: Path::new("/etc/ssl/private"),
            env: &[],
        })
        .err();
    // `/etc/ssl` is a system root, so this one is in the view, which is worth asserting: a
    // view check that refused everything would look like it worked.
    assert!(e.is_none(), "a system root is in the view: {e:?}");

    let e = b
        .wrap(&ConfinePlan {
            cwd: Path::new("/root/somewhere"),
            env: &[],
        })
        .expect_err("a cwd outside the view must refuse");
    let m = format!("{e}");
    assert!(m.contains("not in this session's filesystem view"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
    assert!(
        m.contains("does not contain it"),
        "the refusal must name absence rather than denial: {m}"
    );
}

#[test]
fn the_environment_is_cleared_so_a_token_is_absent_rather_than_filtered() {
    let _only_one = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // Absence beats denial, applied to the environment. An unset-list would have to
    // name every secret-bearing variable anybody will ever invent; a keep-list is
    // wrong in one direction only, and that direction is a visible failure.
    let dir = std::env::temp_dir().join(format!("letibot-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let b = match Bwrap::project(&dir) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return no_boundary(&e, "the_environment_is_cleared");
        }
    };
    let Ok(host) = HostProcesses::confined(&dir, Box::new(b)) else {
        let _ = std::fs::remove_dir_all(&dir);
        return absent(CGROUP);
    };
    // SAFETY: the guard above is held for this whole body, so no other test of this binary is
    // running, and the variable is removed before the guard is dropped.
    unsafe { std::env::set_var("LETIBOT_TEST_FAKE_TOKEN", "s3cr3t-value") };
    let id = host
        .spawn(&SpawnRequest {
            command: "echo path=${PATH:+set}; echo token=${LETIBOT_TEST_FAKE_TOKEN:-absent}; \
                      echo declared=${LETIBOT_DECLARED:-absent}"
                .into(),
            // Nobody named this run: the substrate's own test.
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![("LETIBOT_DECLARED".into(), "on-purpose".into())],
            // No terminal: these are the substrate's own tests, not an operator's run.
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("spawn");
    let _ = host.wait_job(&id, std::time::Duration::from_secs(20));
    let text = host
        .output(&id, 0, usize::MAX)
        .map(|o| o.text())
        .unwrap_or_default();
    unsafe { std::env::remove_var("LETIBOT_TEST_FAKE_TOKEN") };
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        text.contains("token=absent"),
        "the token must not cross: {text}"
    );
    assert!(!text.contains("s3cr3t-value"), "{text}");
    // The keep-list survived, so a command is still a command.
    assert!(text.contains("path=set"), "{text}");
    // A variable the caller declared on purpose crosses, or `--clearenv` would silently drop
    // what a tool asked for.
    assert!(text.contains("declared=on-purpose"), "{text}");
}

/// A secret the caller hands a confined command reaches it, but never through bwrap's argv,
/// which every user on the box reads with `ps`: it travels on `--args` from a descriptor.
#[test]
fn a_secret_crosses_the_boundary_without_being_on_the_command_line() {
    let _only_one = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("letibot-secret-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let secret = (
        "LETIBOT_PROBE_TOKEN".to_string(),
        "s3cr3t-on-fd".to_string(),
    );
    let plain = ("LETIBOT_PLAIN".to_string(), "in-the-open".to_string());
    let env = vec![secret.clone(), plain.clone()];
    let b = match Bwrap::project(&dir) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return no_boundary(&e, "a_secret_crosses_the_boundary");
        }
    };
    let argv = b
        .wrap(&ConfinePlan {
            cwd: &dir,
            env: &env,
        })
        .expect("a command line");
    let joined = argv.join(" ");
    assert!(
        !joined.contains("s3cr3t-on-fd"),
        "the secret is on the argv: {joined}"
    );
    assert!(
        joined.contains("--setenv LETIBOT_PLAIN in-the-open"),
        "{joined}"
    );
    assert!(joined.contains("--args 200"), "{joined}");

    let Ok(host) = HostProcesses::confined(&dir, Box::new(b)) else {
        let _ = std::fs::remove_dir_all(&dir);
        return absent(CGROUP);
    };
    let id = host
        .spawn(&SpawnRequest {
            command:
                "echo token=${LETIBOT_PROBE_TOKEN:-absent}; echo plain=${LETIBOT_PLAIN:-absent}"
                    .into(),
            slug: None,
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env,
            tty: false,
            memory: None,
            env_trailer: false,
        })
        .expect("spawn");
    let _ = host.wait_job(&id, std::time::Duration::from_secs(20));
    let text = host
        .output(&id, 0, usize::MAX)
        .map(|o| o.text())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        text.contains("token=s3cr3t-on-fd"),
        "the secret did not cross: {text}"
    );
    assert!(text.contains("plain=in-the-open"), "{text}");
}

#[test]
fn a_file_written_to_tmp_in_one_confined_call_is_there_for_the_next() {
    // The two-call scratch idiom: with a fresh tmpfs per command a file written by one call is gone by the
    // next, and the shell's complaint reads as a broken shell rather than as the boundary. Run through a
    // plain `/bin/sh`, because a cgroup subtree is different apparatus.
    let dir = std::env::temp_dir().join(format!("letibot-tmp-{}", std::process::id()));
    let project = dir.join("project");
    let session_tmp = dir.join("session-tmp");
    std::fs::create_dir_all(&project).expect("project");
    let project = project.canonicalize().unwrap_or(project);

    let bound = match Bwrap::probe(
        ViewSpec::project_only(&project).with_tmp(TmpView::Session(session_tmp.clone())),
        Egress::Denied,
    ) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return no_boundary(&e, "a_file_written_to_tmp");
        }
    };

    /// One confined `/bin/sh -c`, and what it said.
    fn run<C: Confinement>(c: &C, project: &Path, command: &str) -> (String, i32) {
        let argv = c
            .wrap(&ConfinePlan {
                cwd: project,
                env: &[],
            })
            .expect("a command line");
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .arg("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("the helper runs");
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (said, out.status.code().unwrap_or(-1))
    }

    let (wrote, code) = run(&bound, &project, "echo kept > /tmp/probe.txt; echo wrote");
    assert_eq!(code, 0, "the first call writes: {wrote}");
    let (read, code) = run(&bound, &project, "cat /tmp/probe.txt");
    assert_eq!(code, 0, "the next call must find it: {read}");
    assert!(read.contains("kept"), "{read}");
    // A real file on this box, in the directory the view bound, not something the helper kept
    // in a memory filesystem that happened to survive.
    assert!(
        session_tmp.join("probe.txt").is_file(),
        "the write landed in the session's own directory: {}",
        session_tmp.display()
    );

    // The HOST's `/tmp` is still absent: the view binds the session's directory, never the
    // machine's, which holds other programs' sockets and whatever the operator was doing.
    let host_only = std::env::temp_dir().join(format!("letibot-host-{}", std::process::id()));
    std::fs::write(&host_only, "host bytes").expect("a file of the host's");
    let (absent, _) = run(&bound, &project, &format!("cat {}", host_only.display()));
    assert!(
        !absent.contains("host bytes") && absent.contains("No such file"),
        "the host's /tmp must not be in the view: {absent}"
    );

    // The contrast that makes this a measurement: the default view is unchanged, and a command
    // there still starts with an empty `/tmp`.
    let plain = Bwrap::probe(ViewSpec::project_only(&project), Egress::Denied)
        .expect("the same boundary, without a session /tmp");
    let (gone, _) = run(&plain, &project, "cat /tmp/probe.txt");
    assert!(
        gone.contains("No such file"),
        "a view with no session /tmp is still a fresh tmpfs per command: {gone}"
    );

    let _ = std::fs::remove_file(&host_only);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sessions_tmp_is_created_at_open_and_removed_at_close() {
    // Two facts about the directory: it exists before any command runs, because a `--bind` of a path that
    // is not there fails at every spawn and says nothing about why, and it goes at close so a daemon does
    // not keep one directory per session id for ever.
    use letibot_tools::backend::{ExecBackend, HostBackend};
    use letibot_tools::exec::NoScopes;

    let dir = std::env::temp_dir().join(format!("letibot-stmp-{}", std::process::id()));
    let project = dir.join("project");
    let session_tmp = dir.join("session-tmp");
    std::fs::create_dir_all(&project).expect("project");
    let b = match Bwrap::probe(ViewSpec::project_only(&project), Egress::Denied) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return no_boundary(&e, "a_sessions_tmp_is_created_at_open");
        }
    };
    let host = std::sync::Arc::new(HostProcesses::with_tree(
        &project,
        Box::new(NoScopes::new("this test starts no process")),
    ));
    host.replace_confinement(Box::new(b));
    let backend = HostBackend::executable_with(&project, host)
        .expect("a backend over the fixture root")
        .with_session_tmp(&session_tmp)
        .expect("the session's /tmp is bound");

    assert!(
        session_tmp.is_dir(),
        "the probe makes it, so the bind cannot fail at a spawn: {}",
        session_tmp.display()
    );
    let boundary = backend.boundary().expect("a measured boundary");
    assert_eq!(boundary.view.tmp, TmpView::Session(session_tmp.clone()));
    assert!(
        boundary.view.summary().contains("there for the next"),
        "the disclosure says what it is for: {}",
        boundary.view.summary()
    );

    assert_eq!(
        backend.close(),
        None,
        "a host backend has no sentence to add"
    );
    assert!(
        !session_tmp.exists(),
        "the session is over, so its /tmp goes with it: {}",
        session_tmp.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tmpfs_home_is_declared_with_its_cost_and_a_declared_home_persists() {
    let dir = std::env::temp_dir().join(format!("letibot-home-{}", std::process::id()));
    let project = dir.join("project");
    let home = dir.join("home");
    std::fs::create_dir_all(&project).expect("project");

    let tmpfs = match Bwrap::probe(ViewSpec::project_only(&project), Egress::Denied) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return no_boundary(&e, "a_tmpfs_home_is_declared");
        }
    };
    // The cost is stated, because a build cache that is empty every run is a
    // slowdown somebody will otherwise spend an evening on.
    assert!(
        tmpfs.describe().contains("FRESH TMPFS"),
        "{}",
        tmpfs.describe()
    );
    assert!(
        tmpfs.describe().contains("build cache"),
        "{}",
        tmpfs.describe()
    );

    let declared = Bwrap::probe(
        ViewSpec::project_only(&project).with_home(HomeView::Dir(home.clone())),
        Egress::Denied,
    )
    .expect("a declared home");
    let d = declared.describe();
    assert!(d.contains(&home.display().to_string()), "{d}");
    assert!(!d.contains("FRESH TMPFS"), "{d}");
    // And it is not the operator's home, which is the only property that matters.
    let real_home = std::env::var("HOME").unwrap_or_default();
    assert!(!real_home.is_empty() && home.as_path() != Path::new(&real_home));
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- helpers

/// Everything the model would see: the body and the notes. A test that read only
/// the payload would miss a boundary note, which is where rule 4 lives.
fn payload(r: &letibot_tools::ToolResult) -> String {
    r.render()
}

/// The pids in a scope's cgroup and every cgroup under it, read through the same
/// `cgroup.procs` file the reaper reads, so the membership check and the kill look at one
/// fact and not two.
fn cgroup_members(scope: &letibot_tools::exec::ScopeId) -> Vec<u32> {
    // The tree's own reading: on macOS `cgroup.procs` holds group ids, not members.
    letibot_tools::exec::live_members(scope)
}

// ------------------------------------- the room coordinator is not in the view

/// Run `command` inside a measured boundary and answer its output, or `None` on a box with no
/// boundary (the refusal is asserted by `no_boundary`).
fn run_boxed(view: ViewSpec, cwd: &Path, command: &str) -> Option<String> {
    let b = match Bwrap::probe(view, Egress::Denied) {
        Ok(b) => b,
        Err(e) => {
            no_boundary(&e, "run_boxed");
            return None;
        }
    };
    let argv = b
        .wrap(&ConfinePlan { cwd, env: &[] })
        .expect("a measured boundary wraps");
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the wrapped command runs");
    Some(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

#[test]
fn a_command_inside_the_boundary_cannot_connect_to_or_see_the_room_coordinator() {
    let dir = std::env::temp_dir().join(format!("letibot-roomhide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let sock = dir.join("room-test.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("a coordinator stand-in");
    let probe = format!(
        "python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); print(\"RE\"+\"ACHED\")' {} 2>&1; \
         stat -c kind=%F {} 2>&1",
        sock.display(),
        sock.display()
    );

    // The control: the socket is in the project, so without the rule a command reaches it. A
    // test that only saw the refusal could be measuring a missing python.
    let Some(open) = run_boxed(ViewSpec::project_only(&dir), &dir, &probe) else {
        return;
    };
    assert!(open.contains("REACHED"), "the control must connect: {open}");

    let hidden = ViewSpec::project_only(&dir).hiding(&sock);
    let shut = run_boxed(hidden, &dir, &probe).expect("the boundary measured above");
    assert!(
        !shut.contains("REACHED"),
        "the socket must not connect: {shut}"
    );
    assert!(
        !shut.contains("kind=socket"),
        "and it must not stat as a socket: {shut}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
