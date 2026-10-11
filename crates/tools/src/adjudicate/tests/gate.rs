//! The gate itself: paths, the never-write list, permission rules, asks and disclosures.

use super::*;

/// **R18: a call that names no path is not described by a path it does not have.**
///
/// The facts are what the head prints as `because: …` and what the oracle is shown,
/// and one of them asserted *"the path is inside the session's workspace"* for a
/// call with **no `path` argument at all** — because `path_is_inside` answers `true`
/// when the key is absent. Measured on the operator's screen 2026-09-22: a
/// `job_kill` with no arguments, described by a path nobody had named.
///
/// The requirement's own words: *"a tool call with no path argument is judged on
/// what it DOES, not on a path it does not have — resolving no-target to `/` makes
/// every targetless call a call on the whole filesystem, which is the least true
/// reading available."*
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

    // **A call that does name one keeps the fact, both ways round.** Without this
    // half the test would pass on a gate that had simply stopped saying it.
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

    // **And the workspace fact prints the workspace it was handed.** This is the
    // fact the operator saw on four cards as `workspace: /`; the gate's half of
    // that fix is that it prints what it is given rather than substituting a root,
    // and `a_seat_rooted_at_the_host_still_reports_its_own_workspace` in
    // `backend.rs` is the other half.
    assert!(
        inside.brief().contains("workspace: /w"),
        "the brief must print the workspace it was handed:\n{}",
        inside.brief()
    );
}

/// **At the widest point on the operator's box, the secret stores are still
/// shut.** The operator, adding `automode-edits` (2026-09-17): *"that being
/// said - .ssh and friends must stay protected"*.
///
/// This is the claim the sixth point rests on, so it is checked where it
/// actually holds — at the gate, through the real admit path, with a mode
/// whose `write` disposition is `Admit` and whose decider is a model that
/// would say yes to anything. An ordinary edit goes through with nobody
/// consulted, which is the point of the mode; a write into `.ssh` is refused
/// by `NEVER_WRITE` at step 1, before any point or adjudicator is reached,
/// and a read of a key is refused as a disclosure at step 0.
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

    // The point of the point: an ordinary edit inside the workspace, admitted
    // with nobody asked at all.
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
    // A read that would put a key in the transcript is a disclosure, and no
    // point moves it either.
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

/// **A gate with no guard model refuses to say it is supervising.**
///
/// Answering "ok" and supervising nothing would leave the operator believing
/// every later call was measured while none were — the same lie `Mode::check`
/// refuses to tell by downgrading silently.
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

/// An `allow_session` is not asked twice — **at a point whose grant scope is a
/// session**.
///
/// The mode is named rather than left to the default, and that is the point of the
/// test rather than noise in it: the default is `always-ask`, whose scope is
/// `Once`, where an answer settles that call and nothing else. A test that relied
/// on the default here would have been asserting the old behaviour of a gate that
/// had no notion of where it was sitting.
///
/// The coordinate is one nobody named — write asks, and the answer is remembered —
/// which is also the demonstration that the space is reachable beyond the four.
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

/// **A `Once` point offers the session grant its *Always allow* implies, and
/// the gate honours it.**
///
/// This test once asserted the opposite: the option was withheld at a point
/// whose grants are `Once`, because the mode's contract and the gate had to
/// agree. What unbalanced it was *Always allow* — the one option that
/// reaches past the session was on the ladder at a point that was not
/// supposed to grant past the call. The operator, 2026-09-18: *"if I'm
/// offered to permanently enable — it is stronger than allow_session — so
/// allow_session should always be offered too."* It is offered, and the
/// answer records a grant that settles the next call: a button whose effect
/// the gate would decline is the defect this ladder once shipped, in the
/// other direction.
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

/// **An `allow_session` answer at `always-ask` stands for the session.**
///
/// This test once asserted the opposite, in two stages: first that the grant
/// was silently dropped, then — after the option was withdrawn from the
/// ladder — that naming it was a non-conforming answer and the gate failed
/// closed. Both stages served the mode's contract, *an answer settles that
/// call and nothing else*. The operator retired the contract on 2026-09-18:
/// *"if I'm offered to permanently enable — it is stronger than
/// allow_session — so allow_session should always be offered too."* The
/// durable rule was already on the ladder here; the weaker grant follows it,
/// and an explicit answer is the operator overriding the point's default.
///
/// What the mode still means: the SECOND call is not asked about, because the
/// grant covers it — not because the mode stopped asking. A call outside the
/// grant's class asks again, as the sibling test pins.
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

/// **At `writes allowed` a write is not asked at all**, and the row still exists.
///
/// The second half is the one worth a test: an admission nobody was consulted
/// about is exactly the kind that stops appearing in an audit, and §4c's corpus
/// wants it — *what the operator decided* includes the mode they put this project
/// at.
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
    // Three states, and only one of them is "on". A daemon that printed the
    // same line in all three would be asserting a property it does not have
    // in two of them.
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

    // And the M1 sentence stays true for an M1 session.
    let (state, _, active) = startup_disclosure("none attached", false, false);
    assert_eq!(state, "N/A");
    assert!(!active);
}

/// **The sentence names the classes the session actually seated.**
///
/// The defect this replaced: a caller whose gated tools were `Access::Exec` —
/// the runner role's five job verbs and `monitor`, no `write` anywhere — had to
/// pass `has_write_tools: true` to get the on/off logic right, and the banner
/// then said *"Write tools are callable"* about a session with no write tools.
/// Correct about the boundary and wrong about the session, which is the same
/// shape as the four instances `harnessd`'s `GateWiring` already paid for.
///
/// Nothing here pins a wording. What it pins is that the wording **moves** with
/// the classes, and that a class the session does not have never appears.
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

    // A read-only backend stops a *write* and says so in those terms; for an
    // exec or network seat it is a different mismatch and must not be described
    // as a blocked write.
    let shut_write = startup_disclosure_for("console ask", false, &["write"], true).1;
    assert!(shut_write.contains("cannot reach the disk"), "{shut_write}");
    let shut_exec = startup_disclosure_for("console ask", false, &["exec"], true).1;
    assert_ne!(
        shut_exec, shut_write,
        "an exec seat over a read-only backend is not a blocked write"
    );

    // Empty classes is still the M1 sentence, and it is the only state that
    // may claim nothing can reach the gate.
    assert_eq!(
        startup_disclosure_for("none attached", false, &[], true).0,
        "N/A"
    );
}
