//! **D16 and R98 at the gate.** Two rulings, one question: *may a standing answer settle a
//! command, or must somebody be asked about this one?*
//!
//! * D16 — inside a confinement letibot has **measured**, a per-command ruling protects nothing
//!   a write the mode already admits could not do instead, so at `writes-allowed` a command
//!   there is admitted without a card.
//! * R98 — on a host with **no** boundary, a command that runs the project's own code (a test
//!   runner, a build that executes it, a script in the checkout) asks every time, and no grant
//!   settles it: the file can be rewritten between the answer and the next run.
//!
//! Every fixture here is a boundary built by hand rather than a probed one. A test that wanted
//! a real `bwrap` could not run inside this box's own boundary (no nesting, no cgroup — see the
//! FX note), and what these tests are about is the gate's reading of the measurement, not the
//! measurement. `crates/tools/tests/remote_confine.rs` is where the argv a boundary is built
//! from is measured, and `crates/tools/tests/confine.rs` is where the kernel is.

use super::*;

use crate::exec::{Namespace, NsState, Seal, SealKind};

// ---------------------------------------------------------------------------
// The boundary fixtures
// ---------------------------------------------------------------------------

/// **A boundary that encloses**: every namespace entered, both seals tested and held, egress
/// denied. This is what `Bwrap::probe` refuses to return anything less than, plus the seals
/// being *held* rather than merely not-open. The builder is `measured_boundary` in `tests.rs`,
/// shared with the fixtures whose subject is something a boundary then admits.
fn measured(egress: crate::exec::Egress) -> crate::exec::Boundary {
    crate::exec::Boundary {
        egress,
        ..measured_boundary()
    }
}

fn measured_and_closed() -> crate::exec::Boundary {
    measured(crate::exec::Egress::Denied)
}

/// **An adjudicator that must not be reached.** A pass here is a pass only if nobody was asked,
/// and an assertion on the audit row alone cannot tell *asked and allowed* from *not asked*.
struct Nobody;

impl Adjudicator for Nobody {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        panic!(
            "nobody was to be asked about `{}`, and this adjudicator was",
            req.summary
        );
    }
    fn describe(&self) -> String {
        "nobody".into()
    }
}

/// **An adjudicator that records being asked**, and allows the call once. The record is the
/// assertion: *it asked* is the whole of what a "still asks" test can prove.
#[derive(Default)]
struct Asked(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl Asked {
    fn count(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Adjudicator for Asked {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req.id.clone());
        AdjudicationDecision::selected(req, "allow_once", "human:test", "because")
    }
    fn describe(&self) -> String {
        "human:test".into()
    }
}

/// **An adjudicator that names an option this gate does not offer**, the way an older head or a
/// replayed answer would: the gate must still honour *this call* and record nothing standing.
struct Picks(&'static str);

impl Adjudicator for Picks {
    fn decide(&self, req: &AdjudicationRequest) -> AdjudicationDecision {
        AdjudicationDecision::selected(req, self.0, "human:old-head", "answered by a head")
    }
    fn describe(&self) -> String {
        "human:old-head".into()
    }
}

/// **The admissions a gate told somebody about**, in order.
#[derive(Default)]
struct Told(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl AdmissionSink for Told {
    fn admitted(&self, notice: &AdmissionNotice) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{}: {}", notice.mode, notice.summary));
    }
}

/// A gate at `writes-allowed` on a workspace of `/w`, with the boundary it is handed.
fn gate_at(
    adjudicator: Box<dyn Adjudicator>,
    mode: crate::mode::Mode,
    boundary: Option<crate::exec::Boundary>,
) -> AdjudicatedGate {
    AdjudicatedGate::new(adjudicator)
        .with_identity("s-test", "agent")
        .with_mode(mode)
        .with_boundary(boundary, None)
        .with_surroundings(pinned())
}

fn last_basis(g: &AdjudicatedGate) -> String {
    g.log
        .last()
        .map(|r| r.decision.basis.clone())
        .unwrap_or_default()
}

fn last_by(g: &AdjudicatedGate) -> String {
    g.log
        .last()
        .map(|r| r.decision.by.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// D16 — admitted, and what the row and the notice say
// ---------------------------------------------------------------------------

/// **The ruling, and the three commands the brief names.**
///
/// `cargo test`, a script the session wrote, and `rm -rf target` inside the checkout: all three
/// admitted with nobody asked, the audit row saying why, and a line told to the operator.
#[test]
fn inside_a_measured_boundary_a_command_is_admitted_without_a_card() {
    for (command, region) in [
        ("cargo test --offline -p letibot-tools", "workspace"),
        ("bash scripts/check-fmt.sh main", "workspace"),
        ("rm -rf target", "workspace"),
    ] {
        let told = Told::default();
        let mut g = gate_at(
            Box::new(Nobody),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        )
        .with_admission_sink(Box::new(Told(told.0.clone())));
        let args = json!({ "command": command });
        let call = bash(&args);
        assert_eq!(
            g.admit(&call),
            GateDecision::Admit,
            "`{command}` is inside the measured boundary and must not ask"
        );
        // **The command arranged the way layer A will read it**, so a fixture cannot pass by
        // being a string this classifier places differently from the real one.
        let reading = g.baseline_for(&call);
        assert!(
            reading.regions.iter().any(|r| r.as_str() == region),
            "`{command}` should place a region `{region}`: {:?}",
            reading
                .regions
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(g.log.len(), 1, "one call, one row");
        assert_eq!(g.log[0].effect, "admit");
        assert_eq!(last_by(&g), "gate:boundary");
        assert!(
            last_basis(&g).starts_with("admitted: inside the measured boundary (D16)"),
            "the row has to say why in the ruling's own words: {}",
            last_basis(&g)
        );
        assert!(
            last_basis(&g).contains("bubblewrap 0.11.1 (fixture)"),
            "the row names what measured the boundary: {}",
            last_basis(&g)
        );
        let said = told.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(
            said.len(),
            1,
            "the operator is told once, not only recorded"
        );
        assert!(
            said[0].starts_with("writes allowed: "),
            "the notice says which point admitted it: {}",
            said[0]
        );
    }
}

/// **A second call in the same session is admitted too.** The rule is not *once per session*:
/// what D16 removes is the question, and a first-call-only version would leave the operator
/// answering every build after the first.
#[test]
fn the_admit_is_not_a_once_per_session_thing() {
    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    for _ in 0..3 {
        assert_eq!(
            g.admit(&bash(&json!({ "command": "cargo build --offline" }))),
            GateDecision::Admit
        );
    }
    assert_eq!(g.log.len(), 3);
    assert!(g.log.iter().all(|r| r.decision.by == "gate:boundary"));
}

// ---------------------------------------------------------------------------
// D16 — what still asks
// ---------------------------------------------------------------------------

/// **Everything that crosses the boundary still asks.** `curl` and `cargo fetch` need egress;
/// `git push` publishes, and its intents say so; `gh` is the credential tier. Each is asked
/// even with a measured boundary in force.
#[test]
fn a_command_that_crosses_the_boundary_still_asks() {
    for command in [
        "curl -s https://example.invalid/x",
        "cargo fetch",
        "git push origin main",
        "gh pr create --fill",
    ] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` must still be put to somebody"
        );
        assert_ne!(
            last_by(&g),
            "gate:boundary",
            "`{command}` was settled by the boundary rule:\n{}",
            last_basis(&g)
        );
    }
}

/// **A command that would work outside the checkout still asks** — the grant-that-widens-the-view
/// clause. `/etc/motd` and the operator's home are not in the view, and a ruling about the
/// command would be a ruling about a path the boundary does not contain.
#[test]
fn a_write_outside_the_checkout_still_asks() {
    for command in [
        "echo hi > /etc/motd",
        "rm -rf /home/dead/Projects/other",
        "cp target/x ~/keep",
    ] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` names a path outside the view"
        );
        assert_ne!(last_by(&g), "gate:boundary", "{command}");
    }
}

/// **The mac host shell: no boundary, so nothing is admitted by this rule.** The same three
/// commands that pass above are asked here — this is the fixture the brief calls *a fake
/// backend that reports no confinement*, and it is the state every session on the operator's
/// laptop is in.
#[test]
fn without_a_measured_boundary_the_same_commands_still_ask() {
    for command in [
        "cargo test --offline -p letibot-tools",
        "rm -rf target",
        "make",
    ] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            None,
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` on an unconfined host must ask"
        );
    }
}

/// **A boundary that does not hold is not a boundary.** Three ways for `encloses()` to be
/// false, each measured rather than assumed: a required namespace missing, a seal *open*, and a
/// seal merely unverified. All three still ask, and the last one is the judgement call worth
/// pinning — `complete()` tolerates an unverified seal, and D16's claim does not.
#[test]
fn a_boundary_that_does_not_hold_still_asks() {
    let cases: Vec<(&str, crate::exec::Boundary)> = {
        let mut missing = measured_and_closed();
        missing.ns.insert(
            Namespace::Mount,
            NsState::NotEntered {
                why: "the fixture says the mount namespace did not take".into(),
            },
        );

        let mut open = measured_and_closed();
        open.seals.insert(
            SealKind::NestedNamespaces,
            Seal::Open {
                why: "a child unshared its way back out".into(),
            },
        );

        let mut unverified = measured_and_closed();
        unverified.seals.insert(
            SealKind::NoNewPrivs,
            Seal::Unverified {
                why: "could not be tested from inside".into(),
            },
        );
        vec![
            ("a required namespace missing", missing),
            ("a seal measured open", open),
            ("a seal that could not be tested", unverified),
        ]
    };
    for (what, boundary) in cases {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(boundary),
        );
        let _ = g.admit(&bash(&json!({ "command": "cargo test" })));
        assert_eq!(asked.count(), 1, "{what} must not admit a command");
    }
}

/// **A boundary that shares the host's network is not enclosed either**: something crosses it,
/// and what crosses is what a card is still for.
#[test]
fn a_boundary_with_egress_to_the_host_still_asks() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured(crate::exec::Egress::Host {
            why: "the session seats web_fetch".into(),
        })),
    );
    let _ = g.admit(&bash(&json!({ "command": "cargo test" })));
    assert_eq!(asked.count(), 1);
}

/// **D16 is about `writes-allowed` and says nothing about the other points.** Both directions,
/// because either alone is a test a wrong implementation passes: `always-ask` must still ask
/// (it is the point that is *only* asking), and `allow-all` still admits — but through its own
/// rule, labelled `gate:mode`, so a reader can tell an admit D16 earned from one the point made
/// unconditional.
#[test]
fn the_rule_is_writes_allowed_only() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::ALWAYS_ASK,
        Some(measured_and_closed()),
    );
    let _ = g.admit(&bash(&json!({ "command": "cargo test" })));
    assert_eq!(asked.count(), 1, "always-ask asks about everything");

    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::ALLOW_ALL,
        Some(measured_and_closed()),
    )
    // A daemon turns this on for an unconfined seat (`with_exec_follows_mode(cfg.unconfined)`),
    // and `allow-all`'s own promise is that exec follows the point — without it the call falls
    // through to the adjudicator, which is the operator's 2026-09-11 rule still in force.
    .with_exec_follows_mode(true);
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo test" }))),
        GateDecision::Admit
    );
    assert_eq!(last_by(&g), "gate:mode", "allow-all admits by its own rule");
}

/// **A command the grammar cannot read is not admitted by this rule either.** `encloses()` is
/// necessary and not sufficient: an unreadable action is refused before this, at every point
/// that does not admit everything.
#[test]
fn an_unreadable_command_is_not_admitted_by_the_boundary() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    // An unclosed quote: the grammar cannot read it, so nothing decided about it can be about
    // a boundary.
    let decision = g.admit(&bash(&json!({ "command": "cargo test 'unclosed" })));
    assert!(
        !matches!(decision, GateDecision::Admit),
        "an unreadable command was admitted: {decision:?}"
    );
    assert_eq!(
        g.log.last().map(|r| r.decision.by.as_str()),
        Some("boundary:normaliser")
    );
    assert_eq!(
        asked.count(),
        0,
        "no adjudicator decides about an unread action"
    );
}

// ---------------------------------------------------------------------------
// R98 (1) — on a host with no boundary, the project's own code asks every time
// ---------------------------------------------------------------------------

/// **A durable shape does not settle a test runner on an unconfined host**, and the same cache
/// does still settle something that is not one.
///
/// The shape cache is the durable half of *a standing answer*: it outlives the session and is
/// seeded, on this box, from the operator's own past approvals under this workspace. R98 says
/// the file a runner executes can be rewritten between the answer and the next run, so the
/// shape is not a licence — and the contrast keeps this from passing on a gate that had simply
/// stopped honouring its cache.
#[test]
fn a_durable_shape_does_not_settle_a_test_runner_without_a_boundary() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_unconfined_code_asks(true);
    // Exactly what a human's earlier approval of this shape would have seeded.
    let args = json!({ "command": "cargo test --offline" });
    let seen = g.request_for(&bash(&args));
    let shape = seen.shape.clone().expect("a parsed command has a shape");
    assert_eq!(
        g.seed_shapes([(shape, seen.class.to_string())]),
        1,
        "the fixture must actually seed the shape, or this proves nothing"
    );
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        1,
        "an approval of this shape does not answer the next run"
    );

    // **And the cache is not simply broken.** A shape over a command that runs nothing this
    // project wrote still settles, silently, as it always did. `cargo fmt` is the pair that
    // makes the test bite: same program, same exec access, same tier — and it is not a runner.
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_unconfined_code_asks(true);
    let args = json!({ "command": "cargo fmt --all" });
    let seen = g.request_for(&bash(&args));
    g.seed_shapes([(seen.shape.clone().expect("a shape"), seen.class.to_string())]);
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        0,
        "`cargo fmt` is still settled by its shape"
    );
    assert_eq!(last_by(&g), "gate:shape");
}

/// **The same command inside a measured boundary is not asked twice** — the other side of the
/// same sentence, so the rule above cannot be a rule about `cargo test` in general.
#[test]
fn the_same_runner_is_admitted_inside_the_boundary() {
    let mut g = gate_at(
        Box::new(Nobody),
        crate::mode::Mode::WRITES_ALLOWED,
        Some(measured_and_closed()),
    );
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo test --offline" }))),
        GateDecision::Admit
    );
    assert_eq!(last_by(&g), "gate:boundary");
}

/// **The card says why, and offers no standing rung.** An operator shown *allow for the
/// session* over a call the gate would then refuse to remember is the defect `Offer` exists to
/// prevent, so the ladder narrows to *this one* and the note carries the reason.
#[test]
fn the_card_narrows_the_ladder_and_says_why() {
    let mut g = gate_at(
        Box::new(Asked::default()),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_unconfined_code_asks(true);
    let request = g.request_for(&bash(&json!({ "command": "cargo test --offline" })));
    let ids: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        !ids.contains(&"allow_session") && !ids.contains(&"allow_always"),
        "a standing answer cannot settle this call, so it is not offered: {ids:?}"
    );
    assert!(
        ids.contains(&"allow_once"),
        "the call itself is still decidable"
    );
    let note = request.options_note.clone().unwrap_or_default();
    assert!(
        note.contains("asks every time") && note.contains("`cargo test`"),
        "the card has to say what and why: {note}"
    );
    assert!(
        note.contains("R98"),
        "the note names the ruling it is applying: {note}"
    );

    // **And a command that is not the project's code keeps its full ladder** — otherwise this
    // test would pass on a gate that had narrowed every ladder on every host.
    let plain = g.request_for(&bash(&json!({ "command": "git status"})));
    let ids: Vec<&str> = plain.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        ids.contains(&"allow_session"),
        "`git status` may still be granted for the session: {ids:?}"
    );
    assert_eq!(plain.options_note, None, "and nothing is left off it");
}

/// **A hand-written `allow` rule does not settle it either** — the durable half of "session or
/// durable grant". The rule is left in the config; it simply does not answer this call.
#[test]
fn a_durable_allow_rule_does_not_settle_a_runner_without_a_boundary() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_unconfined_code_asks(true)
    .with_permission(vec![crate::permission::Rule::new(
        "bash",
        "cargo test*",
        crate::permission::Action::Allow,
    )]);
    let _ = g.admit(&bash(&json!({ "command": "cargo test --offline" })));
    assert_eq!(
        asked.count(),
        1,
        "a rule written about the last run does not answer this one"
    );

    // **And it still governs what it can be trusted about**: the same rule over `git status`,
    // which runs nothing this project wrote, is honoured without asking.
    let mut g =
        gate_at(Box::new(Nobody), crate::mode::Mode::WRITES_ALLOWED, None).with_permission(vec![
            crate::permission::Rule::new("bash", "git status", crate::permission::Action::Allow),
        ]);
    assert_eq!(
        g.admit(&bash(&json!({ "command": "git status" }))),
        GateDecision::Admit
    );
    assert_eq!(last_by(&g), "gate:permission");
}

/// **An `allow_once` answer does not seed the shape, so the next run asks again.**
///
/// This is the guard that actually bites. The shape cache is warmed from *any* human admit of a
/// call shape, and without R98 the very first `cargo test` the operator allowed would have
/// settled every later one — which is the shape of the original complaint: the grant was an
/// answer about a file that has since been rewritten.
#[test]
fn an_allow_once_answer_does_not_seed_a_shape_for_a_runner() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_unconfined_code_asks(true);
    let args = json!({ "command": "cargo test --offline" });
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(asked.count(), 1);
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(
        asked.count(),
        2,
        "the shape of the first answer must not settle the second run"
    );
    assert_eq!(last_by(&g), "human:test", "a person answered again");
}

// ---------------------------------------------------------------------------
// R98 (1) — the switch, and what its default is
// ---------------------------------------------------------------------------

/// **With the switch off — the default — an unconfined host behaves exactly as it did before
/// R98 existed.**
///
/// The operator, on the first cut of this rule: *"asking on every build/test on the Mac would
/// just push them to allow-all (which they already run, unsandboxed)"*. So the rule is a switch,
/// and this is the half that says the default is the OLD behaviour, both ways round: a shape
/// seeded from a past approval still settles a runner, and an `allow_session` answer still
/// records a grant that settles the next one.
#[test]
fn the_default_leaves_an_unconfined_host_as_it_was() {
    let asked = Asked::default();
    let mut g = gate_at(
        Box::new(Asked(asked.0.clone())),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    // A shape a previous approval would have seeded — with the switch off it is honoured.
    let args = json!({ "command": "cargo test --offline" });
    let seen = g.request_for(&bash(&args));
    assert_eq!(
        g.seed_shapes([(seen.shape.clone().expect("a shape"), seen.class.to_string())]),
        1
    );
    let call = bash(&args);
    assert_eq!(g.admit(&call), GateDecision::Admit);
    assert_eq!(asked.count(), 0, "the shape answered it, as it always did");
    assert_eq!(last_by(&g), "gate:shape");

    // And the ladder offers both standing rungs, so an answer can still be remembered.
    let mut g = gate_at(
        Box::new(Picks("allow_session")),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    );
    let request = g.request_for(&bash(&json!({ "command": "cargo test" })));
    let ids: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
    assert!(
        ids.contains(&"allow_session") && ids.contains(&"allow_always"),
        "with R98(1) off the ladder is the one it always was: {ids:?}"
    );
    assert_eq!(request.options_note, None, "and nothing is left off it");
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo test" }))),
        GateDecision::Admit
    );
    assert_eq!(g.grants().len(), 1, "the grant was recorded, as before");
}

/// **The always-ask tier survives the boundary, and the first cut of D16 lost it.**
///
/// `sudo -n true` and `shutdown -h now` carry no network intent and name no path, so a rule
/// argued about *egress* and *the view* admitted both inside a measured boundary — the privilege
/// and power lines of the always-ask list, quietly dropped. D16's own words put them back: the
/// admit holds only where the tier is one a point may move at all.
#[test]
fn the_always_ask_tier_still_asks_inside_the_boundary() {
    for command in ["sudo -n true", "shutdown -h now", "crontab /w/cron.txt"] {
        let asked = Asked::default();
        let mut g = gate_at(
            Box::new(Asked(asked.0.clone())),
            crate::mode::Mode::WRITES_ALLOWED,
            Some(measured_and_closed()),
        );
        let _ = g.admit(&bash(&json!({ "command": command })));
        assert_eq!(
            asked.count(),
            1,
            "`{command}` is on the always-ask list and must still be put to somebody"
        );
        assert_ne!(last_by(&g), "gate:boundary", "{command}");
    }
}

/// **A rung the ladder did not offer is refused, and leaves nothing standing behind it.**
///
/// The ladder is the first mechanism (`offer_for`), the request's own option lookup is the
/// second — `admit` refuses an id it did not build — and the recording site's `unconfined_code`
/// check is the third, unreachable while the first two hold. What this pins is the answer a
/// person gets when an older head, a replayed answer or a test names the rung anyway: nothing
/// runs, and no grant is left on the books for the gate to decline later.
#[test]
fn a_rung_the_ladder_did_not_offer_is_refused_for_a_runner() {
    for rung in ["allow_session", "allow_always"] {
        let mut g = gate_at(
            Box::new(Picks(rung)),
            crate::mode::Mode::WRITES_ALLOWED,
            None,
        )
        .with_unconfined_code_asks(true);
        let decision = g.admit(&bash(&json!({ "command": "cargo test" })));
        assert!(
            !matches!(decision, GateDecision::Admit),
            "`{rung}` was honoured although the ladder did not offer it"
        );
        assert!(g.grants().is_empty(), "a grant was recorded for `{rung}`");
        assert!(g.permission().is_empty(), "a rule was written for `{rung}`");
    }

    // **And a command that is not the project's code still offers both rungs** — the same pair
    // of mechanisms, on the side where they are supposed to work.
    let mut g = gate_at(
        Box::new(Picks("allow_session")),
        crate::mode::Mode::WRITES_ALLOWED,
        None,
    )
    .with_unconfined_code_asks(true);
    assert_eq!(
        g.admit(&bash(&json!({ "command": "cargo fmt --all" }))),
        GateDecision::Admit
    );
    assert_eq!(g.grants().len(), 1, "`cargo fmt` may still be granted");
}
