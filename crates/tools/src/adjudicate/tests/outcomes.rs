//! The four outcome classes, at the gate: shell commands, heredocs, durable rules.

use super::*;

/// **A python heredoc that only writes rides the EDIT path** — the operator's ruling of
/// 2026-10-05, asserted where *"permission mechanics"* actually live.
///
/// The ruling, in their words: *"since we try to catch python heredoc edits and render them
/// with diffs - they have to be classified as edits for permission mechanics"*, and the mode
/// they are running answers the other half: *"so we are in allow-edits mode now. there is
/// certain permission set that is allowed and doesnt need oracle. this is to be kept."* So
/// the assertions are (a) the class is `write` and the headline says so, (b) in allow-edits
/// that is the mode's own allowed set and nobody is asked, and (c) in an ask mode the same
/// call asks as a WRITE — while every shape that is not a placed write keeps `exec` and goes
/// on asking in both.
#[test]
fn a_python_heredoc_that_only_writes_is_an_edit_at_the_gate() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let gate = |mode: crate::mode::Mode| {
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human:test",
            move |req: &AdjudicationRequest| {
                a.fetch_add(1, Ordering::Relaxed);
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "fine",
                ))
            },
        )))
        .with_mode(mode)
        .with_exec_follows_mode(false)
        .with_surroundings(pinned());
        (g, asked)
    };

    let write = json!({
        "command": "python3 - <<'PY'\nopen('/w/src/lib.rs', 'w').write('hi')\nPY"
    });

    // **The class, and the headline a person reads.** This call was `exec access` over
    // intents that said `write_file`, which is the same defect as the read one with the
    // consequence the other way round: exec asks at EVERY point, so the edit path — the
    // mode's write rules, the shape cache, the grant vocabulary — was unreachable.
    let (mut g, asked) = gate(crate::mode::Mode::AUTO_EDITS);
    let row = g.request_for(&bash(&write));
    assert_eq!(row.class.access, Access::Write, "{}", row.summary);
    assert!(
        row.summary.contains("write access"),
        "the headline says: {}",
        row.summary
    );
    // **And in allow-edits that is the mode's allowed set.** Nobody is consulted, which is
    // the operator's *"certain permission set that is allowed and doesnt need oracle"*.
    assert_eq!(g.admit(&bash(&write)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "an edit inside the workspace asked its owner"
    );
    let last = g.log.last().expect("a row");
    assert_eq!(last.decision.by, "gate:mode");
    assert!(
        last.decision.basis.contains("admits write calls"),
        "{}",
        last.decision.basis
    );

    // **In ask mode it still asks, and it asks as a write.** The oracle is shown a write
    // and the card says so, which is the point of classifying it rather than of admitting
    // it.
    let (mut g, asked) = gate(crate::mode::Mode::ALWAYS_ASK);
    let row = g.request_for(&bash(&write));
    assert_eq!(row.class.access, Access::Write, "{}", row.summary);
    assert_eq!(g.admit(&bash(&write)), GateDecision::Admit);
    assert_eq!(asked.load(Ordering::Relaxed), 1, "nobody was asked");

    // **Every shape that is not a placed write keeps exec, in both modes.** Each is one of
    // `writes_only`'s guardrails, and each is asserted at the gate because the guardrail is
    // what keeps this narrowing from becoming "any interpreter is an edit".
    for (cmd, why) in [
        (
            "python3 - <<'PY'\nimport subprocess\nsubprocess.run(['ls'])\nPY",
            "a body that shells out",
        ),
        (
            "python3 - <<'PY'\nimport requests\nrequests.get('http://example.com')\nPY",
            "a body that reaches the network",
        ),
        (
            "python3 - <<'PY'\nopen(sys.argv[1], 'w').write('x')\nPY",
            "a write target nobody can place",
        ),
        (
            "python3 - <<'PY'\nopen('/etc/cron.d/x', 'w').write('x')\nPY",
            "a write outside the workspace",
        ),
        (
            "python3 - <<'PY'\nopen('/srv/x.txt', 'w').write('x')\nPY",
            "a write this classifier cannot place",
        ),
        ("python3 deploy.py", "a body nobody read"),
        (
            "bash -c 'echo x > /w/src/lib.rs'",
            "a shell doing the write itself",
        ),
    ] {
        let (mut g, asked) = gate(crate::mode::Mode::AUTO_EDITS);
        let args = json!({"command": cmd});
        let row = g.request_for(&bash(&args));
        assert_eq!(
            row.class.access,
            Access::Exec,
            "{why} was called an edit: {cmd}"
        );
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit, "{cmd}");
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "{why} stopped asking: {cmd}"
        );
    }
}

/// A gate that would admit anything an adjudicator is consulted about, so that a
/// refusal in these tests can only have come from layer A.
fn permissive_gate() -> AdjudicatedGate {
    AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "yes-to-everything",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "yes to everything",
            ))
        },
    )))
    .with_surroundings(pinned())
    .with_trail_source(|_| {
        crate::authorise::AuthorisationTrail::from_messages(
            vec![crate::authorise::Utterance::operator(
                "do whatever you like",
                0,
            )],
            1,
        )
    })
}

#[test]
fn a_disclosure_is_refused_by_a_gate_that_would_otherwise_admit_everything() {
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/cat /home/dead/.ssh/id_rsa"});
    match g.admit(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::Denied { .. },
            tell,
        } => {
            assert!(tell.contains("REFUSED"), "{tell}");
            assert!(tell.contains("Do not retry a variant"), "{tell}");
        }
        other => panic!("a disclosure must not be admitted: {other:?}"),
    }
    assert_eq!(g.log[0].request.tier.as_str(), "blocked");
}

/// **R21 — a shell that only reads is judged on the work, and the four ways it is
/// not.**
///
/// `head-parity-2026-09-21.md` **R21**, raised by the operator looking at a gate on
/// `head`: *"wonder why oracle didnt allow head itself, or why head wasn't
/// categorized as Read."* Both halves had answers and neither was the defect — `head`
/// **is** a read in the classifier, and the oracle was never asked to allow it — and
/// what was left was that **the vehicle outranked the work**: a shell that only reads
/// gated as exec, and the card said *exec access* over intents that said *read*.
///
/// **Measured, 2026-09-22, over every `bash` call this box has gated** (7318 rows,
/// from the store's own `arguments_json`): 1008 rows (13.8%) are read-only by this
/// rule, and **977 of the 5153 rows that were ASKED — 19.0% — stop gating.** The two
/// numbers are the point of this test in the small and of the commit in the large.
///
/// The four guardrails are asserted failing, because the ruling says the rule is
/// *paid for* by the parser's refusals and void the moment a line carries something it
/// could not read. Each of them is a shape the corpus actually holds.
#[test]
fn a_shell_that_only_reads_is_judged_on_the_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // **A fresh gate per case.** A human `allow_session` on one call is a standing
    // permission whose coverage is `intents.is_subset`, so a later call in the same
    // gate can be settled by a grant taken over an earlier one — real behaviour, and
    // noise in a test about which access a call is judged at. One gate per assertion,
    // or the guardrails measure each other.
    let gate = || {
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human:test",
            move |req: &AdjudicationRequest| {
                a.fetch_add(1, Ordering::Relaxed);
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "human:test",
                    "fine",
                ))
            },
        )))
        // `always-ask` is the strongest form of the assertion: clause 4 admits a read
        // at **every** point, including the one whose whole purpose is to ask.
        .with_mode(crate::mode::Mode::ALWAYS_ASK)
        .with_exec_follows_mode(false)
        .with_surroundings(pinned());
        (g, asked)
    };

    // **The yes.** The operator's own example: a `cd`, then a reader. It asked before
    // this, and the card it asked with said `exec access` over `[inspect read_file]`.
    let (mut g, asked) = gate();
    let reads = json!({"command": "cd /w && head -c 2000 src/lib.rs"});
    let row = g.request_for(&bash(&reads));
    assert_eq!(
        row.class.access,
        Access::Read,
        "the class is the vehicle's again: {}",
        row.class
    );
    assert!(
        row.summary.contains("read access"),
        "the headline says: {}",
        row.summary
    );
    assert!(row.summary.contains("head"), "{}", row.summary);
    assert_eq!(g.admit(&bash(&reads)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "nobody is asked about a read inside the boundary"
    );
    // And the row records it as the mode's doing, naming the access the call was
    // judged at rather than the tool's declaration.
    let last = g.log.last().expect("a row");
    assert_eq!(last.decision.by, "gate:mode");
    assert!(
        last.decision.basis.contains("admits read calls"),
        "{}",
        last.decision.basis
    );

    // **The ways it is not a read.** Each keeps the exec class and each still asks —
    // the guardrails are the requirement, so they are asserted as loudly as the yes.
    for (cmd, why) in [
        ("cd /w && frobnicate src/lib.rs", "an unknown program"),
        (
            "cd /w && cat src/lib.rs | tee /w/copy",
            "a pipeline into a writer",
        ),
        (
            "cd /w && cat src/lib.rs > /w/copy",
            "a redirection into a writer",
        ),
        ("cat /etc/passwd", "a read outside the boundary"),
        (
            "cd /w && sed -n 1p src/lib.rs",
            "a program that can execute",
        ),
        ("cd /w && rm -f src/lib.rs", "a deletion"),
        // **A verb that had to be MEASURED to be here.** `git symbolic-ref` reads a ref, so it
        // sat in layer A's read arm — and it was ALSO in the write arm, where the match could
        // never reach it. Source order decided the classification: this call came back
        // `intents ["inspect"]`, `reads_only` true, so `judged_access` narrowed it to
        // `Access::Read` and `Mode::disposition` admits a read at every point. The operator
        // was never asked, and nothing on any screen said so, because what the
        // misclassification suppressed was a PROMPT.
        //
        // It is a write: `git symbolic-ref HEAD refs/heads/other` sets `.git/HEAD` to
        // `refs/heads/other` (measured against a scratch repo), and the man-page scrape in
        // `docs/evidence/man-scrape/` says so in as many words — *"This subcommand can delete
        // symbolic refs that existed before"* — while `FLAG_RULES` already carried `Destroy`
        // for its `-d` spelling. Two signals and neither reached the base arm.
        //
        // Here rather than only in `intent.rs`, because THIS is where the consequence is: the
        // class must be the vehicle's and `asked` must be 1.
        (
            "git symbolic-ref HEAD refs/heads/other",
            "a ref being moved",
        ),
    ] {
        let (mut g, asked) = gate();
        let args = json!({"command": cmd});
        let row = g.request_for(&bash(&args));
        assert_eq!(
            row.class.access,
            Access::Exec,
            "{why} was judged a read: {cmd}"
        );
        assert_eq!(g.admit(&bash(&args)), GateDecision::Admit, "{cmd}");
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "{why} did not reach anybody: {cmd}"
        );
    }

    // **A secret read through a shell is refused, which is stronger than asking.** It
    // is the one case where being judged a read would have been *worse* than being
    // judged exec: `cat ~/.ssh/id_rsa` is a read by every intent it computes, and the
    // thing that stops it is the flow rule, which sees the secret store in the region
    // table and refuses before any point is read. So the boundary conjunct in
    // `reads_only` is what keeps this call's exec access, and the refusal is what
    // happens next.
    let (mut g, asked) = gate();
    let secret = json!({"command": "cat ~/.ssh/id_rsa"});
    let row = g.request_for(&bash(&secret));
    assert_eq!(row.class.access, Access::Exec);
    assert!(matches!(row.tier, Tier::Blocked { .. }), "{:?}", row.tier);
    assert!(matches!(
        g.admit(&bash(&secret)),
        GateDecision::Refuse { .. }
    ));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        0,
        "a secret reached a person"
    );

    // **And a line the parser refused is now a line the POINT decides about**, which is
    // the 2026-10-02 ruling: *"in allow-all, stop refusing a command because the
    // grammar could not resolve a construct"*. It is still not a read — `$(ls)` has no
    // known prefix and `cat` takes a path, so the scope widened to `unbounded` and the
    // boundary conjunct in `reads_only` voids — so it keeps its exec access and the
    // mode is consulted exactly as it would be for the literal spelling.
    //
    // **AND THE MODEL IS THE ONE ASKED** — the operator's ruling of 2026-10-05 overrules
    // what stood here (*"a meaning nobody could read is never handed to a model to
    // approve"*): *"we have to ask oracle for this stuff"*, and *"i want to follow oracle
    // verdict … the rest is oracle."* An unresolved line is now the case a second opinion
    // exists for, so `Adjudicable` IS minted and the oracle answers it — the person is
    // asked when the oracle is unsure (or refuses nothing and has no answer), never
    // because the parser gave up.
    let (mut g, asked) = gate();
    let unresolved = json!({"command": "cd /w && cat $(ls) src/lib.rs"});
    let row = g.request_for(&bash(&unresolved));
    assert_eq!(row.class.access, Access::Exec);
    assert!(
        !row.resolved,
        "the meaning was never resolved and the row says so"
    );
    assert!(
        row.adjudicable().is_some(),
        "the model IS asked about a meaning nobody could read — the ruling of 2026-10-05"
    );
    assert_eq!(g.admit(&bash(&unresolved)), GateDecision::Admit);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "an action nobody could read is answered by the oracle, exactly once"
    );
}

/// **`rm -rf /` is not refused on the strength of the string.** This is the operator's
/// own case, and the test carries no attribute for a while — an insertion landed between
/// its `#[test]` and its `fn`, which left the attribute on the next test's doc comment and
/// this function unreachable. The compiler said so (`never used`) and nobody read it. It is
/// back, because a test that stopped running is worse than a test that fails.
#[test]
fn rm_rf_slash_is_not_blocked_by_the_gate_and_reaches_a_decision() {
    // The operator's case: `rm -rf /` can be allowed if it is the intent. What the
    // gate must NOT do is refuse it on the strength of the string.
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/rm -rf /"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.log[0].request.tier.as_str(), "always_ask");
    assert_eq!(g.log[0].effect, "admit");
}

#[test]
fn an_always_ask_is_never_turned_into_a_standing_grant() {
    // The adjudicator asks for the widest grant it is allowed to name each time. A
    // class grant over a deletion outside the project is the standing authority the
    // fixed list withholds, so the second call must be asked about again.
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let widest = if req.option("allow_session").is_some() {
                "allow_session"
            } else {
                "allow_once"
            };
            Some(AdjudicationDecision::selected(
                req,
                widest,
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let args = json!({"command": "/bin/rm -rf /home/dead/elsewhere"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "an always-ask is asked every time"
    );
    // And it was never even OFFERED a standing grant: the tier is `AlwaysAsk`,
    // whose ladder is `always_ask_options` — no session grant, no durable rule.
    // This half is untouched by the 2026-09-18 revision, which completed the
    // ladder at points that offer the durable rule; the always-ask TIER offers
    // no durable rule and so offers no session grant either.
    assert!(g.log[0].request.option("allow_session").is_none());
    assert!(g.log[0].request.option("allow_always").is_none());

    // An ordinary may-approve exec call, though, now takes the session grant
    // its ladder offers — the operator's revision of 2026-09-18, which retired
    // the exec exception the same way the 2026-09-14 revision had already
    // broken it with the durable rule. The second identical call is settled by
    // the grant, not asked again.
    let ordinary = json!({"command": "/bin/rm -rf /w/target"});
    assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
    assert_eq!(g.admit(&bash(&ordinary)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "the first exec call asked; the grant took the second"
    );
    assert_eq!(
        g.grants().len(),
        1,
        "one standing permission, recorded from the exec answer"
    );
    // And the grant is scoped to what the call showed, not to exec at large: a
    // different program is outside it and asks again.
    // **A program the grant does not name** (R21: `cat` is a read and would be
    // admitted by clause 4 without asking anybody, so it can no longer be the
    // fixture for "a different program asks again").
    let other = json!({"command": "/usr/bin/awk {print} /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&other)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        4,
        "a program the grant does not name asks again"
    );
}

#[test]
/// **Exec asks, and the ladder it asks with is complete.**
///
/// The operator's rule of 2026-09-11 — *"i want something very simple for
/// now - exec and bash always ask me for permission"* — still holds as the
/// DEFAULT: nothing settles an exec call before somebody answers. What
/// changed twice is what an answer can reach. The revision of 2026-09-14 put
/// the durable *Always allow* rule on the ladder; the revision of
/// 2026-09-18 — *"if I'm offered to permanently enable — it is stronger than
/// allow_session — so allow_session should always be offered too"* — put the
/// session grant beside it. The command below is a read inside the workspace,
/// layer A's weakest verdict, because it is exactly the call clause 4 would
/// wave through if the access class let it.
fn exec_class_calls_ask_and_the_ladder_offers_both_standing_options() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let widest = if req.option("allow_session").is_some() {
                "allow_session"
            } else {
                "allow_once"
            };
            Some(AdjudicationDecision::selected(
                req,
                widest,
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    // **`sed -n` and not `cat`** (R21): the command has to be an exec-class call
    // that still ASKS, and a shell that only reads is clause 4 now. This one is the
    // classifier's own exec — see `Baseline::reads_only` — so the ladder is still
    // exercised by a call that reaches it.
    let args = json!({"command": "/bin/sed -n 1p /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the second identical call is settled by the grant the first took"
    );
    assert_eq!(
        g.grants().len(),
        1,
        "one standing permission, recorded from the exec answer"
    );
    assert!(
        g.log
            .iter()
            .all(|r| r.request.option("allow_session").is_some()),
        "exec is offered the session grant, scoped to the program and the class"
    );
    // The revision of 2026-09-14: a DURABLE rule is offered — it is the
    // operator's own preapproval, in a file they read — and it is not a grant.
    assert!(
        g.log
            .iter()
            .all(|r| r.request.option("allow_always").is_some()),
        "exec is offered Always allow, as a rule"
    );
    // And the grant is scoped to the program it was taken over: a different
    // program is outside it and asks again. **Exec, so that asking is what this
    // measures** (R21): `ls` is a look and a shell carrying one is clause 4, which
    // would be admitted without reaching a ladder at all.
    let other = json!({"command": "/usr/bin/awk {print} /w/src/lib.rs"});
    assert_eq!(g.admit(&bash(&other)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "a program the grant does not name asks again"
    );
}

/// **The label names the rule the button writes.**
///
/// The operator's report (2026-09-18): the offer said "add tool and verb to
/// the permissions file" without ever saying **which**, and did not let them
/// adjust it. The derived pattern is in the label now — the same helper the
/// recording site uses, so what is signed and what is written cannot drift —
/// **A gate with a durable sink carries on numbering where it left off.**
///
/// Without this, `next_id` restarts at 1 on every daemon and `INSERT OR
/// IGNORE` drops every colliding row — the whole-session corpus outage
/// measured in the operator's store on 2026-09-20.
#[test]
fn a_resumed_gate_does_not_reissue_request_ids() {
    /// A sink that reports a history, the way a store-backed one does.
    struct Resumed(u64);
    impl crate::authorise::CorpusSink for Resumed {
        fn decided(&self, _row: &crate::authorise::CorpusRow) {}
        fn ruled(&self, _id: &str, _what: &crate::authorise::OperatorOverride) {}
        fn decisions_recorded(&self, _session_id: &str) -> u64 {
            self.0
        }
    }

    let gate = |sink: Option<std::sync::Arc<dyn crate::authorise::CorpusSink>>| {
        let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
            "human",
            |req: &AdjudicationRequest| {
                Some(AdjudicationDecision::selected(
                    req,
                    "allow_once",
                    "h",
                    "fine",
                ))
            },
        )))
        .with_identity("s-42".to_string(), "dead".to_string())
        .with_mode(crate::mode::Mode::WRITES_ALLOWED)
        .with_surroundings(pinned())
        .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
        if let Some(s) = sink {
            g = g.with_corpus_sink(s);
        }
        let _ = g.admit(&bash(&json!({"command": "ls -la"})));
        g.log[0].request.id.clone()
    };

    // A fresh session starts at one.
    assert_eq!(gate(None), "adj-s-42-0001");
    // A session with 646 rows behind it does not reissue any of them.
    let id = gate(Some(std::sync::Arc::new(Resumed(646))));
    assert_eq!(id, "adj-s-42-0647", "a restart re-emitted a stored id");
}

/// **The consented point admits a shell command without asking.**
///
/// The operator, 2026-09-20: *"switched leticl session to allow-all, pressed
/// y, got a warning, still asked about grep"*. The move itself is tested in
/// `wired`; this is the half that matters afterwards — that the point the
/// move lands on actually stops asking about `bash`.
#[test]
fn the_consented_point_admits_a_shell_command_unasked() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::ALLOW_ALL_HERE)
    .with_exec_follows_mode(true)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));

    let args = json!({"command": "cd /home/dead/Projects/leticl && grep -n \"x\" src/"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the operator was asked at a point that admits exec: {}",
        g.log[0].decision.basis
    );
    assert_eq!(g.log[0].decision.by, "gate:mode", "{:?}", g.log[0].decision);
}

/// **The durable rule names the program that needs authority, not the first
/// word of the line.**
///
/// `always_pattern_for_command` took `command.split_whitespace().next()`,
/// which is the program only when the command is one stage. On the
/// operator's screen, 2026-09-20, `cd /home/dead/Projects/letibot/letibot &&
/// python3 - <<'PY' …` offered *Allow `python3` for the session* beside
/// *Always allow `cd*`* — two derivations of the same question, and the
/// durable one, the one written to their config, had the naive answer.
#[test]
fn a_compound_commands_durable_rule_names_the_last_stage_not_the_cd() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let _ = g.admit(&bash(&json!({
        "command": "cd /home/dead/Projects/letibot && cargo test -p letibot-tools"
    })));
    let req = &g.log[0].request;
    let always = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .expect("a segmentable command still offers the durable rule");
    assert!(
        always.label.contains("cargo test*"),
        "names the stage that carries the authority: {}",
        always.label
    );
    assert!(
        !always.label.contains("`cd*`"),
        "offered a permanent rule over `cd`: {}",
        always.label
    );
}

/// **A rule the matcher could never consult is not offered at all.**
///
/// `bash_segments` refuses to split a command carrying a heredoc, and
/// `evaluate_bash` turns that into `Ask` before any rule is read — so for
/// these commands *Always allow* is inert however it is spelled. Offering it
/// invites the operator to sign something durable that does nothing, and
/// then asks them again next time. The rule this seam already states: *"an
/// operator is never shown a button whose effect the gate would then decline
/// to honour."*
#[test]
fn a_heredoc_is_offered_no_durable_rule_because_none_could_match() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let cmd = "cd /home/dead/Projects/letibot && python3 - <<'PY'\nprint(1)\nPY";
    // The premise, asserted rather than assumed: no rule can answer this.
    assert!(
        !crate::permission::a_durable_rule_can_apply(cmd),
        "the premise of this test is that the matcher refuses to segment it"
    );
    let _ = g.admit(&bash(&json!({ "command": cmd })));
    let req = &g.log[0].request;
    assert!(
        req.option("allow_always").is_none(),
        "offered a durable rule that cannot match: {:?}",
        req.options.iter().map(|o| &o.label).collect::<Vec<_>>()
    );
    // The rungs that do work are still there.
    assert!(req.option("allow_once").is_some());
    assert!(req.option("allow_session").is_some());
    assert!(req.option("deny").is_some());
}

/// and a glob typed after `allow_always` in the composer still overrides it.
#[test]
fn the_always_allow_label_shows_the_rule_it_writes() {
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        |req: &AdjudicationRequest| {
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    let _ = g.admit(&bash(&json!({"command": "cargo test -p letibot-tools"})));
    let req = &g.log[0].request;
    let always = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .expect("exec offers the durable rule");
    assert!(
        always.label.contains("cargo test*"),
        "the label names the derived pattern, program and verb: {}",
        always.label
    );
    assert!(
        always.label.contains("permission.json"),
        "the label says where the rule lands: {}",
        always.label
    );
    let session = req
        .options
        .iter()
        .find(|o| o.id == "allow_session")
        .expect("exec offers the session grant");
    assert!(
        session.label.contains("cargo"),
        "the session label names the program it would cover: {}",
        session.label
    );

    // A file tool's rule is its path, and the session grant is the class.
    let _ = g.admit(&call("edit", &json!({"path": "src/lib.rs"})));
    let req = &g.log[1].request;
    let always = req
        .options
        .iter()
        .find(|o| o.id == "allow_always")
        .expect("a write offers the durable rule");
    assert!(
        always.label.contains("src/lib.rs"),
        "the label names the path the rule covers: {}",
        always.label
    );
}

/// *"good old Allow Always"* (the operator, 2026-09-14): the answer writes a
/// rule over the program and its verb, the rule admits the next call before
/// the mode is consulted, `git log; rm x` under `git log*` still asks, and
/// the sink saw the rule once.
#[test]
fn always_allow_on_an_exec_call_becomes_a_prefix_rule_that_is_written_down() {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = asked.clone();
    let written: std::sync::Arc<std::sync::Mutex<Vec<crate::permission::Rule>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let w = written.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_always",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::WRITES_ALLOWED)
    .with_permission(crate::permission::seed())
    .with_permission_sink(std::sync::Arc::new(move |r| {
        w.lock().unwrap().push(r.clone());
        Ok(())
    }))
    .with_surroundings(pinned())
    .with_trail_source(|_| crate::authorise::AuthorisationTrail::from_messages(vec![], 1));
    // `cargo run` is not on the shipped list, so the first call asks.
    let args = json!({"command": "cargo run --bin x"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    let rules = written.lock().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].permission, "bash");
    assert_eq!(rules[0].pattern, "cargo run*");
    drop(rules);
    // The second, different `cargo run` is admitted by the rule, nobody asked.
    let again = json!({"command": "cargo run --bin y -- --flag"});
    assert_eq!(g.admit(&bash(&again)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    // A compound that hides a second command behind the prefix asks.
    let hidden = json!({"command": "cargo run; rm -rf /w"});
    assert_eq!(g.admit(&bash(&hidden)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
    // And the shipped list admits a read-only git without asking at all.
    let ro = json!({"command": "git status --short"});
    assert_eq!(g.admit(&bash(&ro)), GateDecision::Admit);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
}

/// **The operator's own command, at `allow-all`: admitted, with nobody asked.**
///
/// This is the case that filed the ruling, verbatim from their pane:
///
/// ```text
/// Ran "cd … && make lint; echo \"exit=$?\"; …"   · not run · 17 lines
/// this command's meaning does not exist yet, so nothing can decide about it.
/// who decided: boundary:normaliser
/// basis: … parameter_expansion: `$?` … decides the argument: "\"exit=$?\""
/// ...for allow all mode
/// ```
///
/// The `$?` widens nothing — `echo` takes a string, not a place — so the command
/// reads as it would with a literal `$?` replaced by `0`, and the point admits it.
///
/// **Measured at the point that has the rule in it**: `ALLOW_ALL_HERE`, which is
/// `exec: Admit` and `OperatorConsented`, with an adjudicator that FAILS if it is
/// consulted. Nothing is asked, which is the whole of what "for allow all mode"
/// meant. The second half is the other side of the ruling: the same command at
/// `always-ask` is NOT admitted by the point, so a person is asked — the widening is
/// a scope, not a bypass.
#[test]
fn the_operators_echo_dollar_question_is_admitted_at_allow_all_and_asks_elsewhere() {
    use crate::mode::Mode;

    // **`NoAdjudicator` fails closed on everything**, so an `Admit` here can only have
    // come from the point. That is a stronger statement than "nobody was asked": it
    // is that nobody COULD have admitted it and it was admitted anyway.
    let cmd = json!({"command": "cd /w && make lint; echo \"exit=$?\""});

    let mut permissive = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_mode(Mode::ALLOW_ALL_HERE)
        .with_exec_follows_mode(true)
        .with_surroundings(pinned());
    assert_eq!(
        permissive.admit(&bash(&cmd)),
        GateDecision::Admit,
        "allow-all is the point that admits this; the adjudicator refuses everything"
    );
    // And the row still says what was not read, which is the disclosure that replaces
    // the refusal.
    assert!(!permissive.log[0].request.resolved);
    assert!(
        permissive.log[0]
            .request
            .baseline
            .contains("the meaning was never resolved"),
        "the row must still say what was never read: {}",
        permissive.log[0].request.baseline
    );

    // The same command where the point does not admit exec: a person decides, and no
    // model is offered the question.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut narrow = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "deny_and_tell",
                "human:test",
                "not now",
            ))
        },
    )))
    .with_mode(Mode::ALWAYS_ASK)
    .with_exec_follows_mode(false)
    .with_surroundings(pinned());
    assert!(
        matches!(narrow.admit(&bash(&cmd)), GateDecision::Refuse { .. }),
        "a narrow point must not admit it"
    );
    assert_eq!(asked.load(Ordering::Relaxed), 1, "a person was asked");
}

/// **A command the grammar could not read is admitted at `allow-all`, and says so.**
///
/// The operator's case, verbatim from the session store (2026-10-07): a heredoc whose
/// opening line also redirects and pipes. Valid bash; tree-sitter-bash recovers from it
/// at the `|`, so layer A's verdict is `NotRun` — which refused it before any mode was
/// consulted, `allow-all` included. *"im very tired of substitutions - at least for
/// allow-all"*.
///
/// At `allow-all` it now goes on to the mode, which admits it, and the decision says it
/// ran unread. At a point that does not admit everything the refusal is unchanged.
#[test]
fn an_unreadable_command_runs_at_allow_all_and_is_still_refused_elsewhere() {
    use crate::mode::Mode;
    let cmd = json!({"command": "cd /w; python3 - pg.html <<'PY' 2>/dev/null | head -80\nimport sys\nprint(open(sys.argv[1]).read()[:10])\nPY"});

    let mut permissive = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_mode(Mode::ALLOW_ALL_HERE)
        .with_exec_follows_mode(true)
        .with_surroundings(pinned());
    assert_eq!(
        permissive.admit(&bash(&cmd)),
        GateDecision::Admit,
        "allow-all admits it; the adjudicator refuses everything, so only the point could"
    );
    let row = &permissive.log[0];
    assert!(
        row.decision.basis.contains("It ran unread"),
        "the row must say it was not read: {}",
        row.decision.basis
    );

    // Not at a point that admits less: layer A's refusal, before anybody is asked.
    for mode in [Mode::WRITES_ALLOWED, Mode::ALWAYS_ASK] {
        let mut narrow = AdjudicatedGate::new(Box::new(NoAdjudicator))
            .with_mode(mode)
            .with_exec_follows_mode(true)
            .with_surroundings(pinned());
        match narrow.admit(&bash(&cmd)) {
            GateDecision::Refuse { outcome, .. } => assert!(
                matches!(outcome, ToolOutcome::NotRun { ref why } if why.contains("could not read the structure")),
                "{}: {outcome:?}",
                mode.name
            ),
            other => panic!(
                "{}: an unreadable command was not refused: {other:?}",
                mode.name
            ),
        }
    }
}

/// **A standing grant never settles a command whose meaning was never resolved.**
///
/// This is the property the instruction of 2026-10-02 protects — *"that field must
/// stop being derived from it and start being carried on its own"* — and it is the
/// one place the change could have gone wrong invisibly. `Grant::covers` takes
/// `resolved` as an argument and refuses an unresolved action before it looks at
/// coverage (its own unit tests cover that); what those tests cannot see is whether
/// the GATE passes the right thing. It used to derive the flag from the verdict, and
/// the verdict for an unresolvable construct is no longer `NotRun` — so a version that
/// kept deriving it would pass `true` here and an `allow_session` taken over one
/// `cat` would quietly cover every later `cat $ANYTHING`.
///
/// The pair is chosen so the grant CANNOT be refused for an incidental reason: the
/// two calls must have the same class and the same intents, which the assertion says
/// out loud rather than assuming. If that ever stops holding, this test fails on the
/// assertion rather than passing vacuously.
#[test]
fn a_standing_grant_does_not_cover_a_command_whose_meaning_was_never_resolved() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            Some(AdjudicationDecision::selected(
                req,
                "allow_session",
                "human:test",
                "yes, for the session",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::ALWAYS_ASK)
    .with_exec_follows_mode(false)
    .with_surroundings(pinned());

    // First: a resolved call, which the person allows FOR THE SESSION.
    let plain = json!({"command": "cd /w && make lint"});
    let _ = g.admit(&bash(&plain));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "the first call asked once"
    );

    // Second: the same shape with one word the grammar cannot read. Same program,
    // same class, same intents — so only `resolved` can keep the grant off it.
    let opaque = json!({"command": "cd /w && make $TARGET"});
    let resolved_row = g.request_for(&bash(&plain));
    let opaque_row = g.request_for(&bash(&opaque));
    assert_eq!(
        resolved_row.class, opaque_row.class,
        "the pair must differ only in resolution, or this test proves nothing"
    );
    // **THE MODEL IS ASKED NOW**, which is the 2026-10-05 ruling and the reason this
    // assertion changed: a session grant covers a RESOLVED call, and the unresolved twin
    // is answered by the oracle rather than by the point or the person. What the pair
    // still proves is that `resolved` is carried separately — the two requests differ in
    // nothing else — and that the grant itself does not silently cover it.
    assert!(
        opaque_row.adjudicable().is_some(),
        "an adjudicator may answer the unresolved twin — this gate's adjudicator is a \
             person, which is the fixture's wiring and not the ruling"
    );
    let _ = g.admit(&bash(&opaque));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "the session grant must not have settled the unresolved call: it is asked again, \
             and whatever this gate wires answers it"
    );
}

/// **An unresolvable command now reaches the POINT, and the point decides.**
///
/// This test was `an_unresolvable_command_reaching_the_gate_is_not_run_and_never_admitted`
/// and it asserted a refusal. The instruction of 2026-10-02 changed that — *"in
/// allow-all, stop refusing a command because the grammar could not resolve a
/// construct"* — so `/bin/cat $FILE` is admitted at a permissive point, and the two
/// facts that make that safe are what this pins:
///
/// * **`resolved` is still `false`.** It is carried on its own now rather than read
///   off the verdict, so no corpus row claims a resolution that never happened.
/// * **`adjudicable()` is `Some` now** — the operator's ruling of 2026-10-05 (*"the rest
///   is oracle"*) overrules the sentence that stood here (*"a meaning nobody could read
///   is never handed to a MODEL to approve"*). At a permissive point the action is
///   admitted before any gate, so nothing is asked; at a narrow one the oracle answers,
///   and only its UNSURE reaches a person.
#[test]
fn an_unresolvable_command_reaching_the_gate_is_admitted_at_a_permissive_point() {
    let mut g = permissive_gate();
    let args = json!({"command": "/bin/cat $FILE"});
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert!(!g.log[0].request.resolved);
    assert!(
        g.log[0].request.adjudicable().is_some(),
        "the oracle may answer an unread meaning — the ruling of 2026-10-05"
    );
}

/// **And the same command at a point that does not admit it asks whoever this gate
/// wires** — a person, in this fixture.
///
/// The other half of the ruling, and the half that keeps it from being a bypass: the
/// widening is not an admission, it is a *scope*, and a narrow point finds an unbounded
/// scope outside its grant for the same reason it refuses the literal. MEASURED here —
/// `always-ask` with `exec_follows_mode` off, which is the strongest form of the
/// assertion because no point admits exec there.
///
/// **What changed on 2026-10-05 is only whose adjudicator may answer.** The witness is
/// minted for an unresolved action now (the operator: *"the rest is oracle"*), so
/// `adjudicable()` is `Some` — and in THIS gate the adjudicator is `AskAdjudicator`, a
/// person. That is why the test still reads as it does: the witness says *an adjudicator
/// may answer this*, and which adjudicator is the gate's wiring, not the witness's.
#[test]
fn an_unresolvable_command_at_a_narrow_point_reaches_a_person_and_not_a_model() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let a = asked.clone();
    let mut g = AdjudicatedGate::new(Box::new(AskAdjudicator::new(
        "human:test",
        move |req: &AdjudicationRequest| {
            a.fetch_add(1, Ordering::Relaxed);
            // The person is shown the widened scope, which is the point of widening
            // rather than refusing: they can see *what* is unknown.
            assert!(
                req.baseline.contains("unbounded"),
                "the card must say the scope is unbounded: {}",
                req.baseline
            );
            Some(AdjudicationDecision::selected(
                req,
                "allow_once",
                "human:test",
                "fine",
            ))
        },
    )))
    .with_mode(crate::mode::Mode::ALWAYS_ASK)
    .with_exec_follows_mode(false)
    .with_surroundings(pinned());

    let args = json!({"command": "/bin/cat $FILE"});
    let row = g.request_for(&bash(&args));
    assert_eq!(
        row.class.access,
        Access::Exec,
        "not a read: the path is unknown"
    );
    assert!(
        row.adjudicable().is_some(),
        "an adjudicator may answer it — here that adjudicator is the person below"
    );
    // A person is, and their yes is what admits it.
    assert_eq!(g.admit(&bash(&args)), GateDecision::Admit);
    assert_eq!(asked.load(Ordering::Relaxed), 1);
}

#[test]
fn a_bare_command_name_is_not_run_when_the_shell_was_not_declared_fixed() {
    // The alias defeat: `AdjudicatedGate::new` leaves `ShellTrust::Unknown`, so a
    // gate nobody configured refuses a name that something upstream could redefine.
    let mut g = AdjudicatedGate::new(Box::new(NoAdjudicator));
    let args = json!({"command": "ls -la"});
    match g.admit(&bash(&args)) {
        GateDecision::Refuse {
            outcome: ToolOutcome::NotRun { why },
            ..
        } => {
            assert!(why.contains("alias"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}
