use super::*;

/// **The sixth point widens exactly one dial, and the secret stores are not
/// behind it.** The operator, on adding it (2026-09-17): *"that being said -
/// .ssh and friends must stay protected"*.
///
/// Three separate mechanisms keep that true, and none of them is the mode:
/// `Tier::Blocked` answers `false` here at every point; `Tier::AlwaysAsk`
/// answers `false` at every point whose boundary is the operator's; and
/// `NEVER_WRITE` is checked by the gate at step 1, before any point is
/// consulted at all. This pins the two this file owns.
#[test]
fn automode_edits_widens_write_and_nothing_else() {
    let m = Mode::AUTO_EDITS;
    // The one dial that moved, against `automode`.
    assert!(m.admits_unasked(&Tier::MayApprove, Access::Write));
    assert!(!Mode::AUTO.admits_unasked(&Tier::MayApprove, Access::Write));
    // Everything else is `automode`'s, exactly.
    assert_eq!(m.exec, Mode::AUTO.exec);
    assert_eq!(m.network, Mode::AUTO.network);
    assert_eq!(m.decider, Mode::AUTO.decider);
    assert_eq!(m.boundary, Mode::AUTO.boundary);
    assert_eq!(m.requires, Mode::AUTO.requires);
    assert!(!m.admits_unasked(&Tier::MayApprove, Access::Exec));
    assert!(!m.admits_unasked(&Tier::MayApprove, Access::Network));

    // And the two tiers no point on the operator's box may move.
    let blocked = Tier::Blocked {
        rule: crate::adjudicate::FlowRule::SecretOffBox,
        evidence: "a key, leaving".into(),
    };
    let always = Tier::AlwaysAsk {
        rule: "credential_use",
        why: "a key, used".into(),
    };
    for access in [Access::Read, Access::Write, Access::Exec, Access::Network] {
        assert!(
            !m.admits_unasked(&blocked, access),
            "{access:?} at a blocked tier"
        );
        assert!(
            !m.admits_unasked(&always, access),
            "{access:?} at an always-ask tier"
        );
    }
}

/// It is in the ladder, it parses by name, and it does not answer to
/// `acceptEdits` — that name is `writes allowed`'s, and two points under one
/// name would be resolved by list order rather than by meaning.
#[test]
fn the_sixth_point_is_named_and_reachable() {
    assert!(Mode::NAMED.iter().any(|m| m.name == "automode-edits"));
    assert_eq!(
        Mode::parse("automode-edits").unwrap().name,
        "automode-edits"
    );
    assert_eq!(
        Mode::parse("automode_edits").unwrap().name,
        "automode-edits"
    );
    assert_eq!(Mode::parse("acceptEdits").unwrap().name, "writes allowed");
    assert!(
        Mode::parse("automode-edit").is_err(),
        "a near miss is named, not guessed"
    );
}

/// The two tiers a point on the operator's box may not move, at every such point
/// including automode. This is the property the design rests on, so it is checked
/// against the full cross product rather than against the case somebody
/// remembered. `allow-all` is the one point that is not on the operator's box —
/// its prerequisite is a confinement — and it is checked the other way below.
#[test]
fn no_point_on_the_operators_box_moves_always_ask_or_blocked() {
    let ask = Tier::AlwaysAsk {
        rule: "privilege_escalation",
        why: "test".into(),
    };
    let never = Tier::Blocked {
        rule: crate::adjudicate::FlowRule::SecretToTranscript,
        evidence: "test".into(),
    };
    for m in Mode::NAMED
        .iter()
        .filter(|m| m.boundary == Boundary::Operator)
    {
        assert_ne!(m.name, "allow-all");
        for access in [Access::Read, Access::Write, Access::Exec, Access::Network] {
            assert!(
                !m.admits_unasked(&ask, access),
                "{} admitted an always-ask for {access:?}",
                m.name
            );
            assert!(
                !m.admits_unasked(&never, access),
                "{} admitted an blocked for {access:?}",
                m.name
            );
        }
    }
}

/// **The consented point admits the same list the confined one does**, and
/// refuses the same flow rules. The operator, 2026-09-20: *"make it ask for
/// confirmation on bare host and let it thru"* — so `let it thru` has to mean
/// the same thing it means inside a VM, or the confirmation bought them a
/// banner and not a behaviour.
#[test]
fn the_consented_point_admits_the_always_ask_list_and_still_refuses_a_flow_rule() {
    let ask = Tier::AlwaysAsk {
        rule: "privilege_escalation",
        why: "test".into(),
    };
    let never = Tier::Blocked {
        rule: crate::adjudicate::FlowRule::SecretToTranscript,
        evidence: "test".into(),
    };
    assert_eq!(Mode::ALLOW_ALL_HERE.boundary, Boundary::OperatorConsented);
    for access in [Access::Write, Access::Exec, Access::Network] {
        assert!(
            Mode::ALLOW_ALL_HERE.admits_unasked(&ask, access),
            "{access:?}"
        );
        assert!(
            !Mode::ALLOW_ALL_HERE.admits_unasked(&never, access),
            "consent does not repeal the flow rules: {access:?}"
        );
    }
}

/// **The one thing that must stay true of a point that stands for a person's
/// answer: no string reaches it.** A name in a project row, a `--mode` argument
/// or an opencode config is consent nobody gave, replayed by every later daemon
/// — which is the whole failure this point was added to avoid.
#[test]
fn no_string_selects_the_consented_point() {
    assert!(
        !Mode::NAMED
            .iter()
            .any(|m| m.name == Mode::ALLOW_ALL_HERE.name)
    );
    for spelling in [
        "allow-all (this box, consented)",
        "allow-all-here",
        "bypassPermissions",
        "allow-all",
    ] {
        let got = Mode::parse(spelling);
        assert!(
            got.map(|m| m.name != Mode::ALLOW_ALL_HERE.name)
                .unwrap_or(true),
            "`{spelling}` parsed to the consented point"
        );
    }
    // And it drops only the prerequisite it cannot have. A seat with no writable
    // backend still cannot carry it, consent or no consent.
    assert!(!Mode::ALLOW_ALL_HERE.requires.contains(&Prereq::Confinement));
    assert!(
        Mode::ALLOW_ALL_HERE
            .requires
            .contains(&Prereq::WritableBackend)
    );
}

/// The refusal has to name something the reader can change. It named cgroups and
/// bwrap, which on a bare host supply nothing, and the operator went looking
/// there. See `Prereq::how`.
#[test]
fn the_confinement_refusal_names_the_placement_and_the_way_out() {
    let seats = Seats {
        write: true,
        exec: true,
        network: true,
    };
    let why = Mode::ALLOW_ALL
        .check(&[Prereq::WritableBackend], seats)
        .expect_err("no confinement on a bare host");
    assert!(why.contains("firecode"), "{why}");
    assert!(why.contains("/mode allow-all"), "{why}");
    // It may still say "cgroup" — it says it to rule it out. What it must not
    // do is send the reader to build one, which the old text did by naming a
    // probe that supplies this on no host at all.
    assert!(
        why.contains("no cgroup or namespace on this box supplies it"),
        "{why}"
    );
    assert!(!why.contains("`Bwrap` probes"), "{why}");
}

/// The operator's rule, 2026-09-14: *"allow-all should be the true allow-all."*
/// Measured before it: inside a VM, `sudo -n true` and `git clone github.com`
/// each raised a decision nobody was attached to answer. The point requires a
/// confinement, and inside one nothing on the list reaches the operator.
#[test]
fn allow_all_inside_a_boundary_admits_the_always_ask_list() {
    let ask = Tier::AlwaysAsk {
        rule: "privilege_escalation",
        why: "test".into(),
    };
    let never = Tier::Blocked {
        rule: crate::adjudicate::FlowRule::SecretToTranscript,
        evidence: "test".into(),
    };
    assert_eq!(Mode::ALLOW_ALL.boundary, Boundary::Structural);
    assert!(Mode::ALLOW_ALL.requires.contains(&Prereq::Confinement));
    for access in [Access::Write, Access::Exec, Access::Network] {
        assert!(Mode::ALLOW_ALL.admits_unasked(&ask, access), "{access:?}");
        // A secret across the boundary is layer A's refusal, before any point.
        assert!(
            !Mode::ALLOW_ALL.admits_unasked(&never, access),
            "{access:?}"
        );
    }
    // The same coordinate on the operator's box is not allow-all, whatever it
    // is called: the boundary axis is what admits, not the name.
    let on_host = Mode {
        boundary: Boundary::Operator,
        ..Mode::ALLOW_ALL
    };
    assert!(!on_host.admits_unasked(&ask, Access::Exec));
}

/// Clause 4 the other way round: a read never prompts, at any point, and a point
/// that could make one prompt would be a point that breaks the harness by being
/// selected.
#[test]
fn a_read_never_prompts_at_any_point() {
    for m in Mode::NAMED {
        assert!(m.admits_unasked(&Tier::Auto, Access::Read));
        assert!(m.admits_unasked(&Tier::MayApprove, Access::Read));
    }
}

/// **`Tier::Auto` is not a blanket yes**, and this is the case that says why.
///
/// `bash("cat x")` derives `Tier::Auto` — the action is a read inside the
/// boundary — but it arrived here because the *tool* declared exec. A point that
/// admitted it unasked would make `bash` unaskable for every read command, and
/// `bash` is the tool whose result is an arbitrary byte stream. Clause 4 lives in
/// the runtime, which never consults the gate for a read-only tool at all.
#[test]
fn an_auto_tier_over_an_exec_tool_still_asks() {
    assert!(
        !Mode::ALWAYS_ASK.admits_unasked(&Tier::Auto, Access::Exec),
        "a read run through `bash` is still a `bash` call"
    );
    assert!(!Mode::WRITES_ALLOWED.admits_unasked(&Tier::Auto, Access::Exec));
    // And a write tool at writes-allowed does go through, tier notwithstanding.
    assert!(Mode::WRITES_ALLOWED.admits_unasked(&Tier::Auto, Access::Write));
}

/// **The space is fully reachable.** The four are the *named* points, not the only
/// legal ones: a caller may build a coordinate nobody named, which is the whole
/// reason this is a struct with four fields rather than an enum with four
/// variants.
#[test]
fn an_unnamed_coordinate_is_constructible() {
    let strict_writes = Mode {
        name: "ask about writes, remember the answer",
        write: Disposition::Ask,
        grants: GrantScope::Session,
        ..Mode::WRITES_ALLOWED
    };
    assert!(!strict_writes.admits_unasked(&Tier::MayApprove, Access::Write));
    assert_eq!(strict_writes.grants, GrantScope::Session);
    assert!(
        Mode::parse(strict_writes.name).is_err(),
        "an unnamed point is reachable in code and not by name, which is what \
             `NAMED` being a list of four means"
    );
}

/// The operator's own table: writes go through at *writes allowed*, exec still
/// asks there, and both ask at *always-ask*.
#[test]
fn the_named_points_behave_as_the_operator_named_them() {
    let t = Tier::MayApprove;
    assert!(!Mode::ALWAYS_ASK.admits_unasked(&t, Access::Write));
    assert!(!Mode::ALWAYS_ASK.admits_unasked(&t, Access::Exec));

    assert!(Mode::WRITES_ALLOWED.admits_unasked(&t, Access::Write));
    assert!(
        !Mode::WRITES_ALLOWED.admits_unasked(&t, Access::Exec),
        "exec still asks at writes-allowed: §3's transcript choke point does not \
             exist, and `bash` is the tool whose result is an arbitrary byte stream"
    );

    // Automode changes who answers, not what asks.
    assert!(!Mode::AUTO.admits_unasked(&t, Access::Write));
    assert_eq!(Mode::AUTO.decider, Decider::Model);

    assert!(!Mode::READ_ONLY.seats(Access::Write));
    assert!(!Mode::READ_ONLY.seats(Access::Exec));
}

/// An unmet prerequisite refuses **by name**, says how to attach it, and does not
/// hand back a weaker point. The last assertion is the one that matters: there is
/// no `Ok` on this path at all.
#[test]
fn a_missing_prerequisite_refuses_by_name_and_never_downgrades() {
    let seats = Seats {
        write: true,
        exec: true,
        network: false,
    };
    let e = Mode::AUTO
        .check(
            &[
                Prereq::WritableBackend,
                Prereq::ReachableAdjudicator,
                Prereq::Confinement,
            ],
            seats,
        )
        .unwrap_err();
    assert!(e.contains("automode"), "{e}");
    assert!(e.contains("authorisation oracle"), "{e}");
    assert!(
        e.contains("refuses by name") || e.contains("weaker mode"),
        "{e}"
    );
    // And it says how, not only what — which today is a flag, not a build
    // state: the oracle seam exists, so "nothing supplies one" would send the
    // operator to read code instead of passing an argument.
    assert!(e.contains("gatekeeper"), "{e}");

    // Everything present is fine.
    assert!(
        Mode::AUTO
            .check(
                &[
                    Prereq::WritableBackend,
                    Prereq::ReachableAdjudicator,
                    Prereq::Confinement,
                    Prereq::Oracle
                ],
                seats
            )
            .is_ok()
    );
    // Read-only needs nothing, which is why it is always available.
    assert!(Mode::READ_ONLY.check(&[], Seats::default()).is_ok());
}

/// **A prerequisite is about a capability the session will use.**
///
/// The regression this is written against: requiring a writable backend
/// unconditionally made every read-only session refuse to open, because the
/// default point is `always-ask` and the default role seats no write tools. A
/// prerequisite that fires for a capability nobody has is not conservative, it is
/// wrong — and it is the kind of wrong that reads as safety.
#[test]
fn a_read_only_seat_needs_no_writable_backend_whatever_the_point_says() {
    assert!(
        Mode::ALWAYS_ASK.check(&[], Seats::READS_ONLY).is_ok(),
        "a session with nothing to write with needs nowhere to write"
    );
    // And it still needs one the moment write is seated.
    let e = Mode::ALWAYS_ASK
        .check(
            &[Prereq::ReachableAdjudicator],
            Seats {
                write: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(e.contains("writable backend"), "{e}");

    // `automode`'s oracle is not seat-dependent: a point that claims a model
    // answers and has none is `always-ask` wearing a different banner, whatever
    // the session seats.
    let e = Mode::AUTO.check(&[], Seats::READS_ONLY).unwrap_err();
    assert!(e.contains("oracle"), "{e}");
}

/// Automode is **selectable**. A point that is missing reads as "this build cannot
/// do that"; a point that refuses by name reads as "this build can, and here is
/// the piece that is absent".
#[test]
fn automode_exists_and_says_what_it_needs() {
    let m = Mode::parse("automode").expect("automode is a point, not a plan");
    assert_eq!(m.decider, Decider::Model);
    assert!(m.requires.contains(&Prereq::Oracle));
}

#[test]
fn an_unknown_mode_names_the_four_rather_than_defaulting() {
    let e = Mode::parse("yolo").unwrap_err();
    for name in ["read-only", "always-ask", "writes allowed", "automode"] {
        assert!(e.contains(name), "{e}");
    }
    // The spellings a person actually types.
    assert_eq!(
        Mode::parse("writes-allowed").unwrap().name,
        "writes allowed"
    );
    assert_eq!(
        Mode::parse("writes allowed").unwrap().name,
        "writes allowed"
    );
    assert_eq!(Mode::parse("READ_ONLY").unwrap().name, "read-only");
}

/// An unseen directory starts where nothing happens without the operator, and not
/// at read-only — the complaint that started this was a session that could not
/// edit and had no way to change that in place.
#[test]
fn an_unseen_project_starts_at_always_ask() {
    assert_eq!(UNSEEN_PROJECT.name, "always-ask");
    assert!(UNSEEN_PROJECT.seats(Access::Write));
}

/// Every named point's prerequisites are things this crate has a name for, and
/// every prerequisite says how to attach it. A refusal that says what is missing
/// and not how to supply it is one the operator routes around.
#[test]
fn every_prerequisite_says_how() {
    for p in [
        Prereq::WritableBackend,
        Prereq::ReachableAdjudicator,
        Prereq::Oracle,
        Prereq::Confinement,
    ] {
        assert!(!p.as_str().is_empty());
        assert!(p.how().len() > 40, "{:?} has no real instruction", p);
    }
}

/// **A child inherits the point of the session that spawned it.** The operator's
/// ask, in their words: *"make subagent inherit allow-all"* — and it is a sentence
/// about the point, not about the seat: the child keeps its own role, its own tool
/// list and its own downgrade, and only the point travels down.
///
/// Every named point is asserted, including the two that admit everything, because
/// the defect this replaces was a child that came up somewhere else — a permissive
/// parent and children at the point the parent had *opened* at.
#[test]
fn a_child_inherits_the_point_of_the_session_that_spawned_it() {
    let seats = Seats {
        write: true,
        exec: true,
        network: true,
    };
    let have = [
        Prereq::WritableBackend,
        Prereq::ReachableAdjudicator,
        Prereq::Confinement,
        Prereq::Oracle,
    ];
    for m in Mode::NAMED.iter().copied().chain([Mode::ALLOW_ALL_HERE]) {
        let got = m.inherited_by(&have, seats, "coder");
        assert_eq!(got.mode(), m, "`{}` did not travel", m.name);
        assert_eq!(
            got.why(),
            None,
            "`{}` was clamped with nothing to clamp on",
            m.name
        );
    }

    // And the tools are not what travelled: the point comes back whole, and it is
    // the same coordinate — same axes, same boundary, same prerequisites.
    let got = Mode::ALLOW_ALL_HERE.inherited_by(&have, seats, "coder");
    assert_eq!(got.mode().boundary, Boundary::OperatorConsented);
    assert_eq!(got.mode().write, Disposition::Admit);
    assert_eq!(got.mode(), Mode::ALLOW_ALL_HERE);
}

/// **The clamp holds for a seat that cannot carry the point, and it says so by
/// name** — the seat, the point inherited, what was missing, and where the child
/// opened instead. A clamp nobody can read is the silent downgrade under another
/// name, which is the state this file refuses everywhere else.
#[test]
fn a_seat_that_cannot_carry_the_point_gets_the_nearest_one_and_says_so() {
    // A seat with a command and no writable backend — a `runner` on the
    // operator's own box. `allow-all` needs a confinement for what this seat
    // execs, and `Confinement` is read off the placement, so on this box no
    // bwrap probe and no cgroup could have supplied it.
    let seats = Seats {
        write: false,
        exec: true,
        network: false,
    };
    let have = [Prereq::ReachableAdjudicator];
    let got = Mode::ALLOW_ALL.inherited_by(&have, seats, "runner");
    let why = got
        .why()
        .expect("allow-all needs a confinement for an exec seat");
    assert!(why.contains("`runner`"), "the seat is named: {why}");
    assert!(
        why.contains("allow-all"),
        "the point inherited is named: {why}"
    );
    assert!(
        why.contains("confinement"),
        "what was missing is named: {why}"
    );
    assert!(
        why.contains("`writes allowed`"),
        "where it opened instead is named: {why}"
    );
    assert_eq!(got.mode().name, "writes allowed");

    // Not an error and not a downgrade nobody chose: the child opens, at a point
    // this seat's own supplies really do satisfy, and never wider than its parent.
    assert!(got.mode().check(&have, seats).is_ok());
    assert!(got.mode().no_wider_than(Mode::ALLOW_ALL));

    // A seat that CAN carry it is carried, so the clamp is not simply "always
    // narrower": the same seat with the confinement the point asks for keeps
    // `allow-all`, which is the whole point of the mechanism.
    let confined = [
        Prereq::ReachableAdjudicator,
        Prereq::Confinement,
        Prereq::WritableBackend,
    ];
    assert_eq!(
        Mode::ALLOW_ALL
            .inherited_by(&confined, seats, "runner")
            .mode(),
        Mode::ALLOW_ALL
    );
}

/// **The consented point reaches a child of the session that consented.** The
/// confirmation was given for a session and the work it is doing — a child is that
/// work — and the sentence on [`Mode::ALLOW_ALL_HERE`] about it lasting only for
/// the session is about *no file recording it and no later daemon starting here*,
/// not about how many processes one session may run.
#[test]
fn the_consented_point_reaches_a_child_of_the_session_that_consented() {
    // The seat that normally holds it: a coder, whose backend is writable and
    // whose confinement is not — which is exactly what the operator confirmed.
    let seats = Seats {
        write: true,
        exec: false,
        network: false,
    };
    let have = [Prereq::WritableBackend, Prereq::ReachableAdjudicator];
    let got = Mode::ALLOW_ALL_HERE.inherited_by(&have, seats, "coder");
    assert_eq!(got.mode(), Mode::ALLOW_ALL_HERE);
    assert_eq!(got.mode().name, "allow-all (this box, consented)");
    assert_eq!(got.why(), None);

    // And what it does NOT do is survive the seat it was given for: the point
    // still drops only `Confinement`, so a seat with no writable backend cannot
    // carry it — the same rule the consent path in `Harness::set_mode_consented`
    // applies, and the reason the clamp exists at all rather than an error.
    let no_backend = Seats {
        write: true,
        ..Default::default()
    };
    let got =
        Mode::ALLOW_ALL_HERE.inherited_by(&[Prereq::ReachableAdjudicator], no_backend, "coder");
    assert_eq!(got.mode().name, "read-only", "{got:?}");
    assert!(got.why().unwrap().contains("writable backend"));
}

/// **A child of a stricter parent is never more permissive than its parent**, even
/// when its seat could carry more. Authority flows down; the clamp is the whole of
/// that mechanism, and it searches only points that are **no wider** than the one
/// it inherited.
#[test]
fn a_child_of_a_stricter_parent_is_never_more_permissive() {
    // An `automode` parent. The child seats no oracle, so it cannot carry the
    // point — and the widest thing left is not the widest thing there is: a
    // session-wide grant is what `automode` added over `always-ask`, and the
    // clamp may not hand a child a scope its parent never had.
    let seats = Seats::READS_ONLY;
    let have = [Prereq::ReachableAdjudicator];
    let got = Mode::AUTO.inherited_by(&have, seats, "orchestrator");
    assert_eq!(got.mode().name, "always-ask", "{got:?}");
    assert!(got.mode().no_wider_than(Mode::AUTO));
    // The two the clamp must not have reached: both sit ABOVE `automode` on the
    // ladder, and one of them passes `check` for this seat — the comparison that
    // keeps it out is `no_wider_than`, not the prerequisites.
    let rung = |m: Mode| Mode::NAMED.iter().position(|n| n.name == m.name).unwrap();
    assert!(rung(got.mode()) < rung(Mode::AUTO_EDITS));
    assert!(rung(got.mode()) < rung(Mode::ALLOW_ALL));
    assert!(Mode::ALLOW_ALL.check(&have, seats).is_ok());
    // A read-only parent can only ever produce a read-only child, whatever the
    // seat beneath it: it is the floor of the ladder.
    let full = Seats {
        write: true,
        exec: true,
        network: true,
    };
    let all = [
        Prereq::WritableBackend,
        Prereq::ReachableAdjudicator,
        Prereq::Confinement,
        Prereq::Oracle,
    ];
    assert_eq!(
        Mode::READ_ONLY.inherited_by(&all, full, "coder").mode(),
        Mode::READ_ONLY
    );
}

/// **A clamp can never make a point that stands on somebody's answer.**
/// [`Mode::ALLOW_ALL_HERE`] is reachable only by being *carried*: it is not in
/// [`Mode::NAMED`], so it is not on the ladder, and a child whose seat cannot carry
/// it lands on a point that never had a person behind it. Otherwise a VM parent at
/// `allow-all` could hand a host child a consented point nobody consented to.
#[test]
fn a_clamp_never_manufactures_a_point_that_stands_on_a_persons_answer() {
    let seats = [
        Seats::READS_ONLY,
        Seats {
            write: true,
            ..Default::default()
        },
        Seats {
            exec: true,
            ..Default::default()
        },
        Seats {
            write: true,
            exec: true,
            network: false,
        },
    ];
    let haves: [&[Prereq]; 4] = [
        &[],
        &[Prereq::ReachableAdjudicator],
        &[Prereq::WritableBackend, Prereq::Confinement],
        &[
            Prereq::WritableBackend,
            Prereq::ReachableAdjudicator,
            Prereq::Confinement,
            Prereq::Oracle,
        ],
    ];
    for parent in
        Mode::NAMED
            .iter()
            .copied()
            .chain([Mode::ALLOW_ALL_HERE, Mode::ALLOW_ALL, Mode::ALWAYS_ASK])
    {
        for seats in seats {
            for have in haves {
                let got = parent.inherited_by(have, seats, "coder");
                if got.mode().name != Mode::ALLOW_ALL_HERE.name {
                    continue;
                }
                assert_eq!(
                    got.mode(),
                    parent,
                    "`{}` was produced by a clamp over `{}` ({:?}, {:?})",
                    Mode::ALLOW_ALL_HERE.name,
                    parent.name,
                    have,
                    seats
                );
                assert_eq!(got.why(), None);
            }
        }
    }
}

/// **A clamp always lands somewhere the seat can carry** — over the whole cross
/// product, not over the cases somebody remembered. `read-only` is the last rung,
/// requires nothing and is no wider than any point, which is what makes the
/// fallback in `inherited_by` unreachable rather than a guess.
#[test]
fn a_clamp_always_lands_somewhere_the_seat_can_carry() {
    let seats = [
        Seats::READS_ONLY,
        Seats {
            write: true,
            ..Default::default()
        },
        Seats {
            exec: true,
            ..Default::default()
        },
        Seats {
            network: true,
            ..Default::default()
        },
    ];
    let haves: [&[Prereq]; 5] = [
        &[],
        &[Prereq::WritableBackend],
        &[Prereq::ReachableAdjudicator],
        &[Prereq::WritableBackend, Prereq::ReachableAdjudicator],
        &[
            Prereq::WritableBackend,
            Prereq::ReachableAdjudicator,
            Prereq::Confinement,
            Prereq::Oracle,
        ],
    ];
    for parent in Mode::NAMED.iter().copied().chain([Mode::ALLOW_ALL_HERE]) {
        for seats in seats {
            for have in haves {
                let got = parent.inherited_by(have, seats, "coder");
                assert!(
                    got.mode().check(have, seats).is_ok(),
                    "`{}` from `{}` is not something this seat can carry: {:?}",
                    got.mode().name,
                    parent.name,
                    got.why()
                );
                assert!(
                    got.mode().no_wider_than(parent),
                    "`{}` from `{}` is wider than its parent",
                    got.mode().name,
                    parent.name
                );
            }
        }
    }
}

/// **The seat keeps the tools; the point is what travels.** A read-only reviewer
/// seats no write tool, and no point can hand it one — `tools_json` is built from
/// the schemas the role resolved, which is D9's capability boundary rather than a
/// check at call time. What the clamp owes that seat is the other half, and it
/// gives it: the same point is refused the moment the seat has something to write
/// with, because `WritableBackend` is demanded of a seat that writes and of no
/// other.
#[test]
fn a_point_cannot_hand_a_seat_a_tool_it_does_not_have() {
    let have = [Prereq::WritableBackend, Prereq::Confinement];
    let carried = Mode::ALLOW_ALL.inherited_by(&have, Seats::READS_ONLY, "orchestrator");
    assert_eq!(carried.mode(), Mode::ALLOW_ALL);
    assert_eq!(carried.why(), None);
    // A seat with a write tool is a different question, and the same list answers
    // it: the backend is required, and on a box that has none the point is refused
    // by name rather than carried. See `a_read_only_seat_needs_no_writable_backend
    // _whatever_the_point_says` for the other direction of this.
    let e = Mode::ALLOW_ALL
        .check(
            &[Prereq::Confinement],
            Seats {
                write: true,
                ..Default::default()
            },
        )
        .expect_err("a writing seat needs somewhere to write");
    assert!(e.contains("writable backend"), "{e}");
}
