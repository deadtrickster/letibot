//! The gate itself: paths, the never-write list, permission rules, asks and disclosures.

use super::*;

/// A call with no `path` argument is judged on what it does, not on a path it does not have:
/// `path_is_inside` answers `true` for an absent key, so the brief would claim a containment
/// nobody named.
#[test]
fn a_call_with_no_path_is_not_described_by_a_path_it_does_not_have() {
    let mut g = AdjudicatedGate::closed();

    // No `path` key at all: no containment claim, and the absence is stated.
    let no_path = g.request_for(&call("job_kill", &json!({"job": "j1"})));
    let brief = no_path.brief();
    assert!(
        !brief.contains("the path is inside") && !brief.contains("the path is OUTSIDE"),
        "a containment claim about a path nobody named:\n{brief}"
    );
    assert!(
        brief.contains("names no path"),
        "the brief must say the call has no path:\n{brief}"
    );

    // The other half: a call that does name a path keeps the fact, or this would pass on a gate
    // that had stopped saying it.
    let inside = g.request_for(&call("write", &json!({"path": "a/b.txt", "content": "x"})));
    assert!(
        inside
            .brief()
            .contains("the path is inside the session's workspace"),
        "{}",
        inside.brief()
    );
    let outside = g.request_for(&call(
        "write",
        &json!({"path": "/etc/passwd", "content": "x"}),
    ));
    assert!(
        outside
            .brief()
            .contains("the path is OUTSIDE the session's workspace"),
        "{}",
        outside.brief()
    );

    // The workspace fact prints the workspace it was handed rather than substituting a root.
    assert!(
        inside.brief().contains("workspace: /w"),
        "the brief must print the workspace it was handed:\n{}",
        inside.brief()
    );
}

/// The never-write list outranks the widest mode: an ordinary edit is admitted with nobody
/// consulted, while a write into a secret store is refused before any point or adjudicator, and a
/// read of a key is refused as a disclosure.
#[test]
fn the_never_write_list_outranks_the_widest_point_on_this_box() {
    let yes = |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "allow_once",
            "model:test",
            "sure",
        ))
    };
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new("model:test", yes)))
        .with_surroundings(pinned())
        .with_mode(crate::mode::Mode::AUTO_EDITS);

    // An ordinary edit inside the workspace is admitted with nobody asked.
    let ordinary = json!({"path": "/w/src/lib.rs", "content": "fn main() {}"});
    assert!(matches!(
        g.admit(&call("write", &ordinary)),
        GateDecision::Admit
    ));

    // And the stores, which the mode cannot reach.
    for path in [
        "/home/op/.ssh/authorized_keys",
        "/home/op/.gnupg/trustdb.gpg",
        "/home/op/.aws/credentials",
        "/home/op/.config/gh/hosts.yml",
    ] {
        let args = json!({"path": path, "content": "x"});
        let d = g.admit(&call("write", &args));
        assert!(
            !matches!(d, GateDecision::Admit),
            "{path} was admitted at automode-edits: {d:?}"
        );
    }
    // A read that would put a key in the transcript is a disclosure, and no point moves it.
    let read = json!({"command": "cat /home/op/.ssh/id_ed25519"});
    assert!(!matches!(g.admit(&bash(&read)), GateDecision::Admit));
}

/// opencode's `permission` config governs the gate before the mode: `deny`
/// refuses, `allow` admits, `ask` falls through.
#[test]
fn a_permission_config_denies_allows_and_asks() {
    let config = crate::permission::config_to_ruleset(
        &json!({
            // `findLast` semantics: the default `*` comes first, the specific
            // override last.
            "edit": { "*": "ask", "*.secret": "deny" },
            "write": "allow",
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap();
    let mut g = AdjudicatedGate::closed().with_permission(config);

    match g.admit(&call("edit", &json!({"path": "src/keys.secret"}))) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            ..
        } => {}
        other => panic!("a denied permission must refuse, got {other:?}"),
    }

    match g.admit(&call("write", &json!({"path": "x.txt"}))) {
        GateDecision::Admit => {}
        other => panic!("an allowed permission must admit, got {other:?}"),
    }

    // `ask` falls through to the mode; a closed gate fails closed with NotRun.
    match g.admit(&call("edit", &json!({"path": "src/lib.rs"}))) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { .. },
            ..
        } => {}
        other => {
            panic!("an asked permission must fall through (closed -> not_run), got {other:?}")
        }
    }
}

/// Answering "ok" and supervising nothing would leave the operator believing every later call was
/// measured while none were.
#[test]
fn supervision_without_an_advisor_refuses_by_name() {
    let mut g = AdjudicatedGate::new(Box::new(Fixed::new("human:op", true)))
        .with_mode(crate::mode::Mode::ALWAYS_ASK);
    let e = g
        .set_supervision(true)
        .expect_err("nothing to supervise with");
    assert!(e.contains("gatekeeper"), "{e}");
    assert!(!g.supervising());
}

#[test]
fn with_no_adjudicator_a_write_refuses_and_does_not_claim_a_decision() {
    let mut g = AdjudicatedGate::closed();
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("fails closed"), "{why}");
            assert!(why.contains("nobody decided"), "{why}");
        }
        other => panic!("fail closed means refuse, got {other:?}"),
    }
    assert_eq!(g.log.len(), 1);
    assert_eq!(g.log[0].effect, "refuse");
}

/// The mode is named rather than left to the default, because `always-ask` grants `Once`: an
/// answer there settles that call and nothing else, so a session grant would never be exercised.
#[test]
fn an_allow_admits_and_a_session_grant_is_not_asked_twice() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_session",
            "human:test",
            "fine",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode {
        name: "ask about writes, remember the answer",
        write: crate::mode::Disposition::Ask,
        grants: crate::mode::GrantScope::Session,
        ..crate::mode::Mode::WRITES_ALLOWED
    });
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "an allow_session must not be asked again"
    );
    assert_eq!(g.log.len(), 2, "both calls are rows, grant included");
    assert_eq!(g.grants().len(), 1, "and the grant is listable");
}

/// A point whose grants are `Once` still offers the session grant, because `allow_always` on the
/// same ladder is stronger: withholding it is a button whose effect the gate would decline, the
/// same defect in the other direction.
#[test]
fn a_once_scoped_point_offers_the_session_grant_and_the_gate_honours_it() {
    use std::sync::{Arc, Mutex};
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        s.lock()
            .unwrap()
            .extend(req.options.iter().map(|o| o.id.clone()));
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_session",
            "human:test",
            "ok",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);

    let ids = seen.lock().unwrap().clone();
    assert!(
        ids.iter().any(|i| i == "allow_session"),
        "a point whose grants are `Once` must still offer the session grant \
             while it offers the durable rule: {ids:?}"
    );
    assert!(
        ids.iter().any(|i| i == "allow_always"),
        "and the durable rule is still offered: {ids:?}"
    );
    assert!(
        ids.iter().any(|i| i == "allow_once"),
        "allowing this one call must still be offered: {ids:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the second call was settled by the grant, not asked again"
    );
    assert_eq!(g.grants().len(), 1, "the answer stands for the session");
}

/// The durable rule is already on the ladder here, so the weaker grant follows it: an explicit
/// answer overrides the point's default. The mode still means the second call is not asked,
/// because the grant covers it and not because the mode stopped asking.
#[test]
fn at_always_ask_an_allow_session_answer_stands_for_the_session() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_session",
            "human:test",
            "fine",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::ALWAYS_ASK);
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the grant the answer took settles the second call"
    );
    assert_eq!(g.grants().len(), 1, "and the grant is recorded");
}

/// An admission nobody was consulted about is the kind that stops appearing in an audit, so the
/// row has to survive a mode that admits on its own.
#[test]
fn at_writes_allowed_a_write_goes_through_and_is_still_a_row() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "deny",
            "human:test",
            "no",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj)).with_mode(crate::mode::Mode::WRITES_ALLOWED);
    let args = json!({"path": "src/lib.rs"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "nothing was consulted"
    );
    assert_eq!(g.log.len(), 1, "and it is still in the audit");
    assert_eq!(g.log[0].effect, "admit");
    assert!(
        g.log[0].decision.by.contains("mode"),
        "{:?}",
        g.log[0].decision
    );
}

#[test]
fn a_deny_is_denied_and_carries_the_request_id() {
    let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "deny",
            "human:test",
            "not that file",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { req_id },
            ..
        } => {
            assert!(req_id.starts_with("adj-"), "{req_id}");
        }
        other => panic!("a deny is a decision, got {other:?}"),
    }
}

#[test]
fn an_answer_naming_an_option_that_was_not_offered_never_opens_the_gate() {
    let adj = AskAdjudicator::new("test", |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "yes_obviously",
            "model:small",
            "looks fine",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(
                why.contains("non-conforming") || why.contains("did not offer"),
                "{why}"
            );
        }
        other => panic!("a non-conforming answer must not admit: {other:?}"),
    }
}

#[test]
fn the_never_list_is_checked_before_the_adjudicator_and_it_cannot_be_overridden() {
    let consulted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = consulted.clone();
    let adj = AskAdjudicator::new("test", move |req: &AdjudicationRequest| {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(AdjudicationDecision::selected(
            req,
            "allow_always",
            "human:test",
            "yes to everything",
        ))
    });
    let mut g = AdjudicatedGate::new(Box::new(adj));
    for path in [
        ".ssh/authorized_keys",
        "home/x/.aws/credentials",
        ".git/HEAD",
    ] {
        let args = json!({"path": path});
        assert!(
            matches!(g.admit(&call("write", &args)), GateDecision::Refuse { .. }),
            "{path} must be refused"
        );
    }
    assert_eq!(
        consulted.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the never rows are evaluated before dispatch"
    );
}

#[test]
fn nobody_answering_is_a_timeout_and_the_gate_still_refuses() {
    let adj = AskAdjudicator::new("test", |_: &AdjudicationRequest| None);
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs"});
    match g.admit(&call("edit", &args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("timeout"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_console_ask_is_a_real_ask_and_the_brief_is_what_a_human_reads() {
    let out = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    struct Shared(std::sync::Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let adj = ConsoleAdjudicator::new(
        "deadtrickster",
        Box::new(std::io::Cursor::new(
            b"allow_once because I said so\n".to_vec(),
        )),
        Box::new(Shared(out.clone())),
    );
    let mut g = AdjudicatedGate::new(Box::new(adj));
    let args = json!({"path": "src/lib.rs", "old_string": "a", "new_string": "b"});
    assert_eq!(g.admit(&call("edit", &args)), GateDecision::Admit);

    let shown = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    assert!(
        shown.contains("write,host_project,reversible,free"),
        "{shown}"
    );
    assert!(shown.contains("allow_once"), "{shown}");
    assert!(shown.contains("old_string"), "{shown}");
    assert_eq!(g.log[0].decision.basis, "because I said so");
}

#[test]
fn a_long_argument_is_summarised_in_the_brief_rather_than_pasted() {
    let big = "x".repeat(10_000);
    let args = json!({"path": "a.txt", "content": big});
    let mut g = AdjudicatedGate::closed();
    let req = g.request_for(&call("write", &args));
    let brief = req.brief();
    assert!(brief.len() < 4_000, "{} bytes", brief.len());
    assert!(brief.contains("bytes omitted"), "{brief}");
}

#[test]
fn the_startup_disclosure_names_whichever_gate_is_shut() {
    // Three states, only one of them "on": one line for all three would claim a property the
    // daemon has in one of them.
    let (state, detail, active) = startup_disclosure("none attached", true, true);
    assert_eq!(state, "NONE");
    assert!(!active);
    assert!(
        detail.contains("WRITE TOOLS and no adjudicator"),
        "{detail}"
    );

    let (state, detail, active) = startup_disclosure("console ask", false, true);
    assert_eq!(state, "GATE ONLY");
    assert!(!active);
    assert!(detail.contains("READ-ONLY"), "{detail}");

    let (_, detail, active) = startup_disclosure("console ask", true, true);
    assert!(active);
    assert!(detail.contains("takes effect"), "{detail}");

    // And a session with no gated classes at all keeps the "nothing can reach the gate"
    // sentence.
    let (state, _, active) = startup_disclosure("none attached", false, false);
    assert_eq!(state, "N/A");
    assert!(!active);
}

/// The sentence names the classes the session actually seated, so a session with no write tools
/// is not told its write tools are callable. Nothing here pins a wording: what it pins is that the
/// wording moves with the classes, and that a class the session does not have never appears.
#[test]
fn the_disclosure_names_the_classes_seated_and_not_write_by_default() {
    let exec = startup_disclosure_for("console ask", true, &["exec"], true).1;
    assert!(exec.contains("EXEC"), "{exec}");
    assert!(
        !exec.to_lowercase().contains("write"),
        "a session with no write tools was told its write tools are callable: {exec}"
    );

    let both = startup_disclosure_for("console ask", true, &["write", "network"], true).1;
    assert!(both.contains("WRITE"), "{both}");
    assert!(both.contains("NETWORK"), "{both}");
    assert_ne!(both, exec);

    // A read-only backend stops a write and says so; for an exec seat it is a different mismatch
    // and must not be described as a blocked write.
    let shut_write = startup_disclosure_for("console ask", false, &["write"], true).1;
    assert!(shut_write.contains("cannot reach the disk"), "{shut_write}");
    let shut_exec = startup_disclosure_for("console ask", false, &["exec"], true).1;
    assert_ne!(
        shut_exec, shut_write,
        "an exec seat over a read-only backend is not a blocked write"
    );

    // Empty classes is the only state that may claim nothing can reach the gate.
    assert_eq!(
        startup_disclosure_for("none attached", false, &[], true).0,
        "N/A"
    );
}

/// The never-write list is about path arguments, not prose, and about the path a call names
/// rather than the way it spells it: a body quoting the directory is not a write into it, while a
/// symlink in the workspace that lands there is.
#[test]
fn the_never_write_list_is_about_paths_and_not_prose() {
    let yes = |req: &AdjudicationRequest| {
        Some(AdjudicationDecision::selected(
            req,
            "allow_once",
            "model:test",
            "sure",
        ))
    };
    let gate = |surroundings: crate::intent::Surroundings, mode: crate::mode::Mode| {
        AdjudicatedGate::new(Box::new(AskAdjudicator::new("model:test", yes)))
            .with_surroundings(surroundings)
            .with_mode(mode)
    };

    // Content is not a path: the body mentions the directory and the call is admitted.
    let mut g = gate(pinned(), crate::mode::Mode::AUTO_EDITS);
    let notes = json!({"path": "/w/notes.txt", "content": "a note about ~/.ssh is not a path"});
    assert!(
        matches!(g.admit(&call("write", &notes)), GateDecision::Admit),
        "a write whose BODY names the directory is not a write into it"
    );

    // A pattern is not a path either: a `grep` for the literal is a search, not a read.
    let found = json!({"pattern": "~/.ssh", "path": "/w"});
    assert!(
        matches!(g.admit(&call("grep", &found)), GateDecision::Admit),
        "a search for the string is not a read of the directory"
    );

    // The path argument is: the same store is refused at the same mode, and the row names the
    // rule that refused it.
    let into = json!({"path": "/home/dead/.ssh/authorized_keys", "content": "x"});
    match g.admit(&call("write", &into)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            ..
        } => {}
        other => panic!("a write INTO the store must be refused, got {other:?}"),
    }
    let said = g.log.last().expect("a row").decision.basis.clone();
    assert!(said.contains("never-write list"), "{said}");

    // A symlink into the store is the store, for every file tool, and nothing in the arguments
    // says so.
    let dir = std::env::temp_dir().join(format!(
        "letibot-gate-ssh-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let home = dir.join("home");
    let ws = dir.join("ws");
    let store = home.join(".ssh");
    std::fs::create_dir_all(&store).expect("the store");
    std::fs::write(store.join("id_ed25519"), b"k").expect("a key");
    std::fs::create_dir_all(&ws).expect("the workspace");
    std::os::unix::fs::symlink(&store, ws.join("keys")).expect("a link into the store");
    let mut surroundings = pinned();
    surroundings.home = Some(home.display().to_string());
    surroundings.workspace = Some(ws.display().to_string());
    let ws_s = ws.display().to_string();
    let mut g = gate(surroundings, crate::mode::Mode::ALLOW_ALL);
    // The name is not itself a key name: `id_ed25519` would be refused one step earlier as a
    // disclosure, which is a different rule. What is under test is the path a link lands in.
    for (name, args) in [
        ("read", json!({"path": "keys/some-file"})),
        ("write", json!({"path": "keys/new-file", "content": "x"})),
        (
            "edit",
            json!({"path": "keys/some-file", "old_string": "k", "new_string": "j"}),
        ),
        ("grep", json!({"pattern": "PRIVATE", "path": "keys"})),
        ("glob", json!({"pattern": "some-*", "path": "keys"})),
    ] {
        let called = GateCall {
            name,
            access: Access::Read,
            args: &args,
            turn_id: "t1",
            call_id: "c1",
            workspace: ws_s.as_str(),
            target_exists: Some(true),
            scripts: &[],
        };
        match g.admit(&called) {
            GateDecision::Refuse {
                outcome: ToolOutcome::Denied { .. },
                ..
            } => {}
            other => {
                panic!("`{name}` through a link into the store must be refused, got {other:?}")
            }
        }
        let said = g.log.last().expect("a row").decision.basis.clone();
        assert!(
            said.contains("forbidden to touch"),
            "the refusal names the operator's rule: {said}"
        );
    }

    // A shell command is best effort: an absolute path in the words is refused, a relative one
    // is not, because the gate cannot know what it resolves to from inside that shell.
    let command = format!("cat {}", ws.join("keys/some-file").display());
    let args = json!({ "command": command });
    let called = GateCall {
        name: "bash",
        access: Access::Exec,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: ws_s.as_str(),
        target_exists: None,
        scripts: &[],
    };
    match g.admit(&called) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            ..
        } => {}
        other => panic!("an absolute path into the store must be refused, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}
