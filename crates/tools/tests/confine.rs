//! Acceptance tests for layer 1's namespace half: the boundary that was
//! **measured**, the refusals when there is none, and the two properties the
//! design says the mount view exists for.
//!
//! # Why there is no `#[ignore]` in this file
//!
//! Same discipline as `tests/exec.rs`. A host that cannot make a user namespace is
//! a real host and the Ubuntu default since 24.04 —
//! `kernel.apparmor_restrict_unprivileged_userns=1` transitions a freshly created
//! userns into a profile that denies `CAP_SYS_ADMIN`, so the `uid_map` write fails
//! and the namespace is inert. On such a box the interesting half of this file is
//! the refusal, and a skip would be a green suite that measured nothing about the
//! boundary it is named after.
//!
//! So every test that needs a real boundary asserts on **both** branches: either
//! the property, or that the refusal names what was missing and that nothing ran
//! unconfined.

use std::path::{Path, PathBuf};

use letibot_tools::exec::confine::{
    BWRAP_ENV, Boundary, ConfinePlan, Confinement, Egress, Grant, HomeView, Namespace,
    NoConfinement, NsState, Seal, SealKind, Unconfined, ViewSpec,
};
use letibot_tools::exec::{Bwrap, ExecError, HostProcesses, ProcessHost, ScopeKind, SpawnRequest};
use letibot_tools::testing::{confined_harness, confined_harness_with};
use letibot_transcript::ToolOutcome;

/// The branch a test took, printed so a green run says which half of the world it
/// was measured against.
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

/// The assertion for the branch where this box cannot confine. It is not "nothing
/// to see here": the refusal has to name what was missing and has to say that
/// nothing was run, because the whole entry is that a boundary which silently is
/// not there is worse than none.
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
}

// ------------------------------------------------------- rule 1: fail closed

#[test]
fn a_session_that_asks_for_a_boundary_and_has_none_refuses_and_never_runs_unconfined() {
    // **The rule that outranks the rest, tested for real rather than on this
    // kernel's happy path.** The helper is pointed at a path that is not there,
    // which is what a host without bubblewrap looks like from inside the code.
    let e = Bwrap::probe(ViewSpec::project_only("/"), Egress::Denied);
    // Guard the fact, not the proxy: point the lookup at nothing and re-probe.
    // SAFETY: this test does not spawn threads before restoring the variable.
    unsafe { std::env::set_var(BWRAP_ENV, "/nonexistent/bwrap") };
    let refused = Bwrap::probe(ViewSpec::project_only("/tmp"), Egress::Denied);
    unsafe { std::env::remove_var(BWRAP_ENV) };
    drop(e);

    let err = refused.expect_err("a helper that is not there cannot confine");
    let m = format!("{err}");
    assert!(m.contains("/nonexistent/bwrap"), "{m}");
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
        eprintln!("no cgroup v2 subtree here; the confinement refusal is still asserted above");
        return;
    };
    let before = host.scopes().len();
    let e = host
        .spawn(&SpawnRequest {
            command: "echo this must not run".into(),
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
        })
        .expect_err("a host with no boundary must not spawn");
    let m = format!("{e}");
    assert!(m.contains("no boundary on this host"), "{m}");
    assert!(m.contains("was NOT run"), "{m}");
    assert!(host.jobs().is_empty(), "nothing may be recorded as running");
    // The scope opened for the refused JOB is gone. The session and turn scopes
    // legitimately remain — they are opened on the way and are not the job's —
    // which is why this counts job scopes rather than scopes, the difference the
    // first spelling of this assertion got wrong.
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
        eprintln!("no cgroup v2 subtree here");
        return;
    };
    // The path a tool actually takes: `spawn` fails, and `bash` renders it.
    let e = host
        .spawn(&SpawnRequest {
            command: "true".into(),
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
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
    let c = ProcessHost::confinement(host.as_ref()).expect("a confined host has a confinement");
    let b = c.boundary().expect("a measured boundary");

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

    // And the inode is not the harness's own — which is the comparison that makes
    // the readback a measurement rather than a formality.
    for n in Namespace::REQUIRED {
        let mine = std::fs::read_link(format!("/proc/self/ns/{}", n.proc_name()))
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if let Some(NsState::Entered { inode }) = b.ns.get(&n) {
            assert_ne!(inode, &mine, "{n} inside must differ from the harness's");
        }
    }

    // The disclosure names the mechanism with its version, not a category.
    let d = c.describe();
    assert!(d.contains("bubblewrap"), "{d}");
    assert!(d.contains("entered ("), "{d}");
}

#[test]
fn a_partial_boundary_describes_itself_as_partial_through_the_whole_stack() {
    // **The test the brief asks for by name**, driven from the top rather than from
    // a fixture struct: what a caller reads has to become partial when the state is.
    //
    // The mechanism: a `Confinement` that reports a boundary with one non-required
    // namespace missing. Everything above it — `HostProcesses::describe`,
    // `HostBackend::describe` — must carry that through, because none of them holds
    // a constant about confinement any more.
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

    // Build it from a real measurement where we can, so the partial case is a real
    // boundary minus one namespace rather than an invention.
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
        eprintln!("no cgroup v2 subtree here");
        return;
    };
    let d = ProcessHost::describe(&host);
    assert!(d.contains("PARTIALLY"), "the host must say partial: {d}");
    assert!(d.contains("NOT ENTERED"), "and which one: {d}");
    assert!(d.contains("ipc"), "{d}");
    // The required ones are still there, so it does still confine — the disclosure
    // has to be able to say both things at once rather than collapsing to a
    // boolean.
    assert!(!d.contains("NOT CONFINED"), "{d}");
}

#[test]
fn the_backend_disclosure_no_longer_hard_codes_a_confinement_claim() {
    // `HostBackend::describe` shipped saying `read-only` for the writable backend
    // and `UNSANDBOXED EXEC` for every exec backend. Both were constants asserting
    // a session property. This asserts the second one is now read, by checking that
    // two different confinements produce two different disclosures.
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
        Err(e) => eprintln!("no cgroup v2 subtree here: {e}"),
    }

    // The confined one: the **same function**, and a different sentence, because
    // the sentence is read off the state rather than written into the match arm.
    match HostBackend::confined(&dir) {
        Ok(b) => {
            let d = b.describe();
            assert!(d.contains("EXEC"), "{d}");
            assert!(
                !d.contains("NOT CONFINED"),
                "a confined backend must not describe itself as unconfined: {d}"
            );
            assert!(d.contains("confined"), "{d}");
            assert!(d.contains("bubblewrap"), "the mechanism, named: {d}");
        }
        Err(e) => {
            let m = format!("{e}");
            eprintln!("no confined backend on this host: {m}");
            assert!(
                m.contains("was NOT run") || m.contains("nothing would reap it"),
                "the refusal must say nothing ran: {m}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unconfined_is_a_declared_state_and_not_an_absent_one() {
    // The third of the three states. It must be distinguishable from "asked for and
    // missing", and the difference has to reach a reader.
    let Ok(host) = HostProcesses::new(std::env::temp_dir()) else {
        eprintln!("no cgroup v2 subtree here");
        return;
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
    // **The point of the entry.** Not "the read was refused" — there is nothing to
    // refuse, because there is nothing there.
    //
    // # The secret is a fixture, and that is a finding rather than a convenience
    //
    // The first spelling of this test read the operator's own `~/.ssh/id_rsa`, and
    // it never reached the boundary: `NEVER_WRITE` **denied the whole `bash` call**
    // because the string `.ssh` occurred in the command text. That is
    // `docs/boundary-and-adjudication.md` §3's first defect, live — *"it is a
    // string check, so it is wrong in both directions"* — and it fires here on a
    // read, in a session where the file is not in the mount namespace at all and
    // therefore could not have been read by any spelling.
    //
    // So the fixture is a secret this test creates, outside the project, with no
    // `.ssh` in its name. What it measures is the boundary; the string check is
    // somebody else's file and is reported rather than edited.
    let mut h = confined!("a_secret_outside_the_project_is_absent");
    let outside = std::env::temp_dir().join(format!("letibot-secret-{}", std::process::id()));
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::write(
        outside.join("credential.pem"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n",
    )
    .expect("fixture secret");

    // TWO calls, because there are two mechanisms and one would hide the other.
    //
    // Layer A's flow rule (`letibot_tools::intent`) landed after this test was
    // written, and it classifies a `.pem` read whose stdout surfaces as a disclosure
    // — so a single call naming `credential.pem` is now refused BEFORE the boundary
    // is reached, and this test's own comment predicted exactly that: *"the string
    // check denies the call before the boundary is reached, so the boundary's
    // behaviour would go unmeasured."*
    //
    // So the boundary is measured on a file that is not credential-shaped, and layer
    // A is measured separately below. Both hold, and neither is standing in for the
    // other.
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

    // The second mechanism, in front of the first: naming the credential at all is
    // refused by the flow rule, and nothing runs. Belt and braces — the boundary
    // above already made the bytes unreachable.
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

    // No key bytes, by any spelling. The assertion is on the marker rather than on
    // the filename, because the filename is the thing a path list gets wrong.
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
    // The half that makes the boundary usable rather than merely safe — and the
    // path has to be the SAME inside and out, or every compiler diagnostic names a
    // file the read tools cannot open.
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
    // And it landed on the operator's real disk, read without going through the
    // backend: a test that asked the tool whether it wrote is a test of the tool's
    // opinion of itself.
    assert_eq!(h.read_file("written.txt").trim(), "confined-write");
}

#[test]
fn a_sibling_project_is_not_in_the_view() {
    // The property the *project*-scoped part of "project-scoped" buys: one project,
    // not "the operator's code".
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
            // Respelled without `$(…)` and `$?`: layer 2 reports both as
            // unresolvable — a value that does not exist until the shell runs — and
            // an unresolvable command is `not_run` before it reaches the boundary.
            // The measurement is unchanged; `sed` does the labelling the command
            // substitution was doing, and `||` the labelling `$?` was.
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
    let c = ProcessHost::confinement(host.as_ref()).expect("confinement");
    assert!(c.describe().contains("DENIED"), "{}", c.describe());
    assert_eq!(c.boundary().map(|b| b.egress.clone()), Some(Egress::Denied));

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
    // The case that must not be reported as a defect. `Egress::Host` shares the
    // host's network namespace deliberately, and the record has to say *decision*
    // rather than *not entered* — otherwise an authorised `git push` looks like a
    // boundary failure and a real boundary failure looks like somebody's choice.
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
    // **The hole the sandboxing survey caught, run rather than reasoned about.**
    // `unshare(CLONE_NEWUSER)` is available to an unprivileged process; the caller
    // gets full capabilities in the namespace it just made, and that is enough to
    // `unshare(CLONE_NEWNS)` past the mount view its parent built. So mount, PID
    // and network namespaces are decoration unless this is closed.
    let mut h = confined!("a_child_cannot_unshare_its_way_out");
    let host = h.processes.as_ref().expect("host").clone();
    let c = ProcessHost::confinement(host.as_ref()).expect("confinement");
    let b = c.boundary().expect("measured");

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
        c.describe().contains("seal nested-namespaces"),
        "the disclosure must state the seal: {}",
        c.describe()
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
    // The reference launcher sets `PR_SET_NO_NEW_PRIVS` before restricting because
    // an unprivileged restrict is rejected without it — an ordering fact, not a
    // style. bubblewrap does the equivalent; this asserts the RESULT, because a
    // helper's documented behaviour is the helper's word.
    let h = confined!("no_new_privs_is_read");
    let host = h.processes.as_ref().expect("host").clone();
    let c = ProcessHost::confinement(host.as_ref()).expect("confinement");
    let b = c.boundary().expect("measured");
    match b.seals.get(&SealKind::NoNewPrivs) {
        Some(Seal::Held { how }) => assert!(how.contains("NoNewPrivs: 1"), "{how}"),
        Some(Seal::Unverified { why }) => eprintln!("no-new-privs unverified here: {why}"),
        other => panic!("no-new-privs must not be open in a boundary handed back: {other:?}"),
    }
}

// --------------------------------------------- rule 4: absence must be legible

#[test]
fn a_path_outside_the_view_produces_a_boundary_note_and_not_a_bare_enoent() {
    // **The empty-haystack case.** `cat: /elsewhere/thing: No such file or
    // directory` is read by a model as an answer about the world. It is not.
    //
    // The path is a fixture rather than `~/.ssh/id_rsa` for the reason recorded in
    // `a_secret_outside_the_project_is_absent`: a check that denies the call before
    // the boundary is reached leaves the boundary's behaviour unmeasured. That now
    // includes layer A's flow rule, which reads `.pem` as credential-shaped — hence
    // `thing.txt` here.
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
        all.contains("another spelling"),
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
    // §3's subtle case, as an executable statement of the decision. `ssh`
    // legitimately reads `~/.ssh/id_rsa`, and the resolution is that the key is
    // never made readable: an agent socket is forwarded instead, so the bytes are
    // *usable* without being *readable-into-context*.
    //
    // The assertion is about the API surface rather than about a run, because the
    // decision is a property of what can be spelled: there is no `Grant` variant
    // that binds a key file, and `agent_from_env` refuses anything that is not a
    // socket.
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

    // And a `ReadOnly` grant of the same key, which IS spellable, must announce the
    // consequence rather than hiding it — this is the one path by which somebody
    // could make a key readable, and it cannot be taken quietly.
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
        eprintln!("no cgroup v2 subtree here");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let id = host
        .spawn(&SpawnRequest {
            command: format!(
                "echo sock=$SSH_AUTH_SOCK; test -S \"$SSH_AUTH_SOCK\" && echo is-a-socket; \
                 cat {}/id_rsa 2>&1; true",
                keydir.display()
            ),
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![],
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
    // The join happens on the HOST, before the namespaces exist, because
    // `/sys/fs/cgroup` is deliberately not in the view. This asserts the ordering
    // held: a confined process is a member of its scope and `cgroup.kill` reaches
    // it across the pid namespace.
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
    let Ok(b) = Bwrap::project(std::env::temp_dir()) else {
        eprintln!("no usable boundary on this host");
        return;
    };
    let e = b
        .wrap(&ConfinePlan {
            cwd: Path::new("/etc/ssl/private"),
            env: &[],
        })
        .err();
    // `/etc/ssl` IS a system root, so this one is in the view — which is itself
    // worth asserting, because a view check that refused everything would look
    // like it worked.
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
    // Absence beats denial, applied to the environment. An unset-list would have to
    // name every secret-bearing variable anybody will ever invent; a keep-list is
    // wrong in one direction only, and that direction is a visible failure.
    let dir = std::env::temp_dir().join(format!("letibot-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let Ok(b) = Bwrap::project(&dir) else {
        eprintln!("no usable boundary on this host");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let Ok(host) = HostProcesses::confined(&dir, Box::new(b)) else {
        eprintln!("no cgroup v2 subtree here");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    // SAFETY: set before the spawn, removed after; this test spawns a process, not
    // a thread that reads the environment.
    unsafe { std::env::set_var("LETIBOT_TEST_FAKE_TOKEN", "s3cr3t-value") };
    let id = host
        .spawn(&SpawnRequest {
            command: "echo path=${PATH:+set}; echo token=${LETIBOT_TEST_FAKE_TOKEN:-absent}; \
                      echo declared=${LETIBOT_DECLARED:-absent}"
                .into(),
            cwd: ".".into(),
            scope: ScopeKind::Turn,
            scope_name: None,
            background: false,
            env: vec![("LETIBOT_DECLARED".into(), "on-purpose".into())],
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
    // The keep-list survived, so a command is still a command...
    assert!(text.contains("path=set"), "{text}");
    // ...and a variable the caller declared on purpose crosses, because otherwise
    // `--clearenv` would silently drop what a tool asked for.
    assert!(text.contains("declared=on-purpose"), "{text}");
}

#[test]
fn a_tmpfs_home_is_declared_with_its_cost_and_a_declared_home_persists() {
    let dir = std::env::temp_dir().join(format!("letibot-home-{}", std::process::id()));
    let project = dir.join("project");
    let home = dir.join("home");
    std::fs::create_dir_all(&project).expect("project");

    let Ok(tmpfs) = Bwrap::probe(ViewSpec::project_only(&project), Egress::Denied) else {
        eprintln!("no usable boundary on this host");
        let _ = std::fs::remove_dir_all(&dir);
        return;
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
/// `cgroup.procs` file the reaper reads — so the membership check and the kill are
/// looking at one fact and not two.
fn cgroup_members(scope: &letibot_tools::exec::ScopeId) -> Vec<u32> {
    fn walk(dir: &Path, out: &mut Vec<u32>) {
        if let Ok(s) = std::fs::read_to_string(dir.join("cgroup.procs")) {
            out.extend(s.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    walk(&e.path(), out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&scope.path, &mut out);
    out
}
