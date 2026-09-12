//! **What a session can actually reach**, and what it says about it.
//!
//! Everything under test here was built, tested, and unreachable before this file
//! existed: nine exec tools, two write tools, the intent group, the namespace
//! boundary, the authorisation trail, the denial sink and the monitor wake all had
//! passing unit tests and no constructor. The unit tests proved the parts work; this
//! proves an invocation can get to them, and — more importantly — that one which
//! passes no flag still cannot.
//!
//! # What it needs, and what it does not
//!
//! The vocabulary GGUF, because `Harness::open` renders the stable prefix and a
//! dialect that does not fit the vocabulary is exactly the kind of failure that is
//! silent at run time. **Not the model server**: nothing here generates. Seating,
//! confining, refusing and disclosing are all decided before a token is produced,
//! and a test that needed a generation to prove them would be measuring the box's
//! load. `LETIBOT_VOCAB_GGUF` overrides the path.
//!
//! # Why several of these assert on prose
//!
//! Because the prose is the deliverable. A refusal that does not say what to attach
//! costs the reader a source dive, and `docs/tool-design-brief.md` §3's *"errors
//! carry the fix"* is a rule about the sentence, not about the `Err`. What is
//! asserted is that the fix is **in** the sentence, never a particular wording.

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tools::Adjudicator;

fn config(seat: Seat) -> Config {
    // The workspace is this repository: `HostBackend::new` canonicalises its root,
    // so it has to be a directory that exists, and a tree with files in it is what
    // a seated `read` would be pointed at anyway.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut cfg = Config::for_this_box(repo);
    cfg.dialect = Dialect::Qwen;
    cfg.seat = seat;
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    cfg
}

fn parts(cfg: &Config) -> Parts {
    Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    )
}

/// An adjudicator that would admit anything, and never gets the chance to.
///
/// It exists so a *seating* test does not need a human at a terminal. It is not a
/// permissive default sneaking in through a test helper: nothing in these tests
/// runs a turn, so nothing reaches it, and `Adjudicator::describe` is what the
/// refusal check reads — this one names itself, which is the whole difference
/// between it and `NoAdjudicator`.
struct Attached;

impl Adjudicator for Attached {
    fn decide(
        &self,
        req: &letibot_tools::AdjudicationRequest,
    ) -> letibot_tools::AdjudicationDecision {
        letibot_tools::AdjudicationDecision::unavailable(
            req,
            "test",
            "this adjudicator exists to be attached, not to decide",
        )
    }
    fn describe(&self) -> String {
        "test adjudicator (attached, decides nothing)".into()
    }
}

fn open(cfg: Config, adj: Option<Box<dyn Adjudicator>>) -> Result<(), String> {
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    Harness::open_with(&p, cfg, hub, adj, None)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 1. Nothing widens by default
// ---------------------------------------------------------------------------

/// **The invocation that passes no flag gets what it got before.**
///
/// This is the rule the whole strand is under, and the way it regresses is not a
/// decision — it is somebody adding a tool to the default role because the role now
/// has somewhere to put it. Six tools, all `Read`, over a backend that cannot write.
#[test]
fn the_default_session_is_read_only_and_seats_exactly_what_it_did() {
    let cfg = config(Seat::default());
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open(&p, cfg, hub).expect("the default session opens with no flags");
    let w = h.wiring();

    assert_eq!(w.role, "orchestrator");
    assert_eq!(
        w.seated,
        vec!["read", "grep", "glob", "ask_code", "ask_corpus", "read_spill"],
        "the default tool set changed; that is a re-prefill for every stored session \
         as well as a capability change"
    );
    assert!(!w.has_write_tools, "seated: {:?}", w.seated);
    assert!(!w.has_exec_tools, "seated: {:?}", w.seated);
    assert!(!w.has_network_tools, "seated: {:?}", w.seated);
    assert!(!w.backend_writable);
    assert!(
        w.adjudicator.starts_with("none"),
        "a read-only session needs no adjudicator and must not claim one: {}",
        w.adjudicator
    );

    // The one thing that IS new for a default session, and it is instrumentation
    // rather than capability: the encoder measures what turns did. It changes no
    // behaviour on this seat — the prose half is off, and the tool-declared half
    // cannot fire because `todo` and `goal` are not seated.
    assert!(
        w.intent_encoder,
        "the intent encoder is attached for every session; without it every \
         completion is `NoEncoder` and the reason is ours"
    );
}

/// The registry is a **superset** and the role decides what the prompt carries.
///
/// The failure this rules out is the one §8.4 names: a build that registers ten
/// tools and quietly seats eight. `write` is registered for every session now, so
/// the thing that keeps it out of a default prompt is the role and nothing else.
#[test]
fn a_registered_tool_the_role_does_not_name_is_not_in_the_prompt() {
    let cfg = config(Seat::default());
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open(&p, cfg, hub).expect("opens");
    for absent in ["write", "edit", "bash", "job_list", "monitor", "web_search", "todo"] {
        assert!(
            !h.wiring().seated.iter().any(|s| s == absent),
            "`{absent}` reached a default session's prompt"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The gate refuses to start rather than starting and failing every call
// ---------------------------------------------------------------------------

/// **A write seat with nobody to decide does not open.**
///
/// `docs/tool-design-brief.md` §2.5 is about a gate that says allowed because
/// nothing is wired. This is its inverse, and it costs the same way: the session
/// starts, the banner prints, the operator types, and one turn later every call
/// comes back `not_run`. Meanwhile the model has been told in its **stable prefix**
/// that it has `write` and `edit`, which is the harness lying to it in the one place
/// that cannot be corrected without a full re-prefill.
#[test]
fn a_write_seat_with_no_adjudicator_refuses_to_start_and_says_what_to_attach() {
    let e = open(
        config(Seat::Coder),
        Some(Box::new(letibot_tools::NoAdjudicator)),
    )
    .expect_err("a coder seat with `NoAdjudicator` must not open");

    // The fix is in the sentence, not in a source file the reader has to find.
    assert!(e.contains("write") || e.contains("edit"), "{e}");
    assert!(
        e.contains("orchestrator"),
        "the refusal must name the alternative seat: {e}"
    );
    assert!(
        e.contains("adjudicator"),
        "the refusal must name what is missing: {e}"
    );
    // And it must say why refusing to *start* is different from refusing to *run*,
    // because "it fails closed already" is the objection this answers.
    assert!(e.contains("not_run"), "{e}");
}

/// The same seat opens once somebody is attached, and the wiring says all three
/// seams are on — not because a constructor was called, but because the gate and
/// the ledger were asked.
#[test]
fn a_write_seat_with_an_adjudicator_opens_fully_wired() {
    let cfg = config(Seat::Coder);
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
        .expect("a coder seat with an adjudicator opens");
    let w = h.wiring();

    assert_eq!(w.role, "coder");
    assert!(w.seated.iter().any(|s| s == "write"));
    assert!(w.seated.iter().any(|s| s == "edit"));
    assert!(w.has_write_tools);
    assert!(
        w.backend_writable,
        "a coder seat over a read-only backend is `GATE ONLY`: an admitted write \
         cannot reach the disk"
    );
    assert!(
        w.trail_installed,
        "without a trail source the adjudicator decides on `NotCollected` — nobody \
         looked, which is not the same fact as the operator having said nothing"
    );
    assert!(
        w.denials_surfaced,
        "without a denial sink a refusal reaches the model and stops there, which is \
         §4b's whole chain"
    );
    assert!(!w.adjudicator.starts_with("none"), "{}", w.adjudicator);

    // And the banner reflects it. Read from the wiring, so this cannot pass while
    // the session is something else.
    let lines: Vec<String> = h
        .config()
        .disclosures(w)
        .iter()
        .map(|d| d.to_string())
        .collect();
    let all = lines.join("\n");
    assert!(all.contains("write"), "{all}");
    assert!(all.contains("auth trail"), "{all}");
}

// ---------------------------------------------------------------------------
// 3. `bash` is off even behind the role that owns it
// ---------------------------------------------------------------------------

/// **The runner seats eight of its nine tools, and the ninth needs its own flag.**
///
/// If this box cannot build a confined backend the test says so and stops rather
/// than passing: `HostBackend::confined` fails rather than degrading, and a seating
/// test that silently skipped when the boundary was unavailable would be reporting
/// the health of `bwrap`.
#[test]
fn the_runner_role_does_not_seat_bash_without_its_own_flag() {
    let mut cfg = config(Seat::Runner);
    assert!(!cfg.allow_bash, "the default must not be a shell");
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let opened = Harness::open_with(&p, cfg.clone(), hub, Some(Box::new(Attached)), None);

    let h = match opened {
        Ok(h) => h,
        Err(e) => {
            let e = e.to_string();
            // The refusal has to name which half is missing, and has to be a
            // refusal rather than a fall back to `HostBackend::executable`.
            assert!(
                e.contains("cgroup") || e.contains("boundary") || e.contains("confin"),
                "a confined backend failed for a reason it did not name: {e}"
            );
            assert!(
                e.contains("refusal") || e.contains("coder"),
                "the refusal must say what to do instead: {e}"
            );
            eprintln!(
                "SKIPPING THE SEATING HALF: this box cannot build a confined backend, \
                 and that is the correct outcome rather than an unconfined one. {e}"
            );
            return;
        }
    };

    let w = h.wiring();
    assert!(
        !w.seated.iter().any(|s| s == "bash"),
        "`bash` was seated without --bash: {:?}",
        w.seated
    );
    for present in ["job_list", "job_output", "job_wait", "job_kill", "monitor"] {
        assert!(
            w.seated.iter().any(|s| s == present),
            "`{present}` is shaped output and belongs on this seat: {:?}",
            w.seated
        );
    }
    assert!(w.has_exec_tools, "the job verbs are `Access::Exec`");

    // The absence is a disclosure, not a silence, and it says why rather than
    // "for safety".
    let all = h
        .config()
        .disclosures(w)
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("bash"), "{all}");
    assert!(
        all.contains("transcript"),
        "the reason `bash` is off is the transcript edge, and saying anything vaguer \
         makes it look like caution rather than a named hole: {all}"
    );

    // With the flag, it is seated. Same everything else.
    cfg.allow_bash = true;
    let hub2 = Hub::new("s-bash");
    cfg.session_id = "s-bash".into();
    let h2 = Harness::open_with(&p, cfg, hub2, Some(Box::new(Attached)), None)
        .expect("the runner seat opened a moment ago");
    assert!(
        h2.wiring().seated.iter().any(|s| s == "bash"),
        "--bash did not seat it: {:?}",
        h2.wiring().seated
    );
}

/// **The same for `coder`, which is the seat an operator actually sits in.**
///
/// `m2_coder` names `bash` as of 2026-09-11 and the daemon strips it back off
/// unless `--bash` is passed, exactly as it does for the runner. Two seats, one
/// rule, and it is asserted twice because the strip is written per-seat: a
/// `Seat::Coder` arm that forgot the `retain` would still pass the runner's test.
///
/// The grants are checked here too. They are the half that makes a seated shell
/// able to build — `$HOME` inside the view is a fresh tmpfs, so `~/.cargo` is
/// ABSENT without one — and a grant that does not reach the boundary is a session
/// that looks equipped and cannot compile.
#[test]
fn the_coder_role_does_not_seat_bash_without_its_own_flag_and_grants_reach_the_view() {
    let mut cfg = config(Seat::Coder);
    assert!(!cfg.allow_bash, "the default must not be a shell");
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let opened = Harness::open_with(&p, cfg.clone(), hub, Some(Box::new(Attached)), None);

    let h = match opened {
        Ok(h) => h,
        Err(e) => {
            let e = e.to_string();
            assert!(
                e.contains("cgroup") || e.contains("boundary") || e.contains("confin"),
                "a confined backend failed for a reason it did not name: {e}"
            );
            eprintln!("SKIPPING THE SEATING HALF: this box cannot build a boundary. {e}");
            return;
        }
    };
    assert!(
        !h.wiring().seated.iter().any(|s| s == "bash"),
        "`bash` was seated on `coder` without --bash: {:?}",
        h.wiring().seated
    );

    // With the flag and the toolchain granted.
    cfg.allow_bash = true;
    cfg.grants_ro = vec![
        std::path::PathBuf::from(std::env::var("HOME").expect("a home")).join(".cargo"),
    ];
    cfg.session_id = "s-coder-bash".into();
    let hub2 = Hub::new(&cfg.session_id);
    let h2 = Harness::open_with(&p, cfg, hub2, Some(Box::new(Attached)), None)
        .expect("the coder seat opened a moment ago");
    let w = h2.wiring();
    assert!(
        w.seated.iter().any(|s| s == "bash"),
        "--bash did not seat it on `coder`: {:?}",
        w.seated
    );
    assert!(w.has_exec_tools, "`bash` is `Access::Exec`");

    // And the grant is in the boundary the session will actually run under, named
    // with its consequence — read-only in the view still means readable into the
    // transcript, and a disclosure that omits that is the decision without its cost.
    let all = h2
        .config()
        .disclosures(w)
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains(".cargo"), "the grant is not in the disclosure: {all}");
    assert!(
        all.contains("READABLE INTO CONTEXT"),
        "a grant must be disclosed with what it costs: {all}"
    );
}

// ---------------------------------------------------------------------------
// 4. The seats that were built and had no constructor
// ---------------------------------------------------------------------------

/// D9's plan mode, and the two tools that make a plan a plan.
///
/// The interesting assertion is the negative one: plan mode is *"no writes to the
/// **work**"*, not "no writes", and a planner that could not record its plan would
/// have to carry it in the context it is about to hand over.
#[test]
fn the_planner_seat_can_write_its_plan_and_not_the_work() {
    let cfg = config(Seat::Planner);
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
        .expect("the planner seat opens");
    let w = h.wiring();
    assert!(w.seated.iter().any(|s| s == "write_plan"));
    assert!(w.seated.iter().any(|s| s == "say"));
    assert!(w.seated.iter().any(|s| s == "todo"));
    assert!(!w.seated.iter().any(|s| s == "write"), "{:?}", w.seated);
    assert!(!w.seated.iter().any(|s| s == "edit"), "{:?}", w.seated);
    assert!(
        w.backend_writable,
        "`write_plan` is a scoped write and still a write: over a read-only backend \
         it is `GATE ONLY` and the plan never reaches the disk"
    );
    assert!(w.has_network_tools, "`say` is `Access::Network`");
}

/// The researcher seat carries the web tools, and they refuse rather than
/// pretending — which is a *different* thing from not being seated.
#[test]
fn the_researcher_seat_carries_seams_that_refuse_by_name() {
    let cfg = config(Seat::Researcher);
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
        .expect("the researcher seat opens");
    let w = h.wiring();
    assert!(w.seated.iter().any(|s| s == "web_search"));
    assert!(w.seated.iter().any(|s| s == "web_fetch"));
    assert!(w.has_network_tools);
    // And the banner says nothing is behind them, computed from the seams rather
    // than asserted about them.
    let all = h
        .config()
        .disclosures(w)
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("web"), "{all}");
}

// ---------------------------------------------------------------------------
// 5. A denial the operator can see
// ---------------------------------------------------------------------------

/// **§4b, end to end through the parts that will actually carry it.**
///
/// `docs/boundary-and-adjudication.md` §4b is a requirement, and the chain is what
/// makes it one: the gate denies, the operator is not told, the model infers *the
/// approach was wrong* rather than *the action was forbidden*, tries a variant, and
/// the task dies with the operator seeing only a dead task. Step 2 is what this
/// closes.
///
/// The gate, the sink and the hub are the real ones. Only the adjudicator is a stub,
/// and it is `NoAdjudicator` — the honest worst case, where nobody decided at all.
#[test]
fn a_refusal_reaches_the_operators_log_at_the_moment_it_is_decided() {
    use letibot_harnessd::harness::HubDenials;
    use letibot_sessionlog::SessionEvent;
    use letibot_tools::{Access, AdjudicatedGate, Gate, GateCall, GateDecision, NoAdjudicator};

    let hub = Hub::new("s-denial");
    let mut gate = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_identity("s-denial", "dead")
        .with_denial_sink(Box::new(HubDenials::new(hub.clone())));

    let args = serde_json::json!({"command": "rm -rf /home/dead/Projects/letibot/target"});
    let decision = gate.admit(&GateCall {
        name: "bash",
        access: Access::Exec,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: None,
    });
    assert!(
        matches!(decision, GateDecision::Refuse { .. }),
        "a gate with nothing behind it must refuse"
    );

    let denials: Vec<(String, String, String, String)> = hub
        .retained()
        .into_iter()
        .filter_map(|e| match e.event {
            SessionEvent::DenialRaised {
                tool,
                outcome,
                grant,
                by,
                ..
            } => Some((tool, outcome, grant, by)),
            _ => None,
        })
        .collect();
    assert_eq!(
        denials.len(),
        1,
        "before this seam was wired the refusal reached the model and nobody else, \
         which is the whole defect"
    );
    let (tool, outcome, grant, by) = &denials[0];
    assert_eq!(tool, "bash");
    // **`not_run` and `denied` must never collapse.** Nobody decided here, and
    // reporting it as a denial would claim a decision that was not made.
    assert_eq!(
        outcome, "not_run",
        "`NoAdjudicator` answers `Unavailable`; calling that a denial claims \
         somebody decided"
    );
    assert!(
        !by.is_empty(),
        "a refusal that does not say who decided is the one that costs an hour"
    );
    // The grant path arrives WITH the denial, not after the task has died: a
    // refusal that can only be routed around teaches people to route around
    // refusals, and one whose grant arrives late is that with an extra step.
    assert!(
        grant.contains("adj-"),
        "the operator must be able to grant it by name: {grant}"
    );
}

/// The denial is **durable**: a head attaching after the refusal replays it, rather
/// than joining a session where a task stopped for no visible reason.
#[test]
fn a_denial_is_replayed_to_a_head_that_was_not_there() {
    use letibot_sessionlog::SessionEvent;
    let hub = Hub::new("s-late");
    hub.publish(SessionEvent::DenialRaised {
        request_id: "adj-1".into(),
        turn_id: "t1".into(),
        call_id: "c1".into(),
        tool: "write".into(),
        summary: "write(path: /etc/hosts)".into(),
        baseline: "1 path; outside the workspace".into(),
        by: "human:dead".into(),
        basis: "outside the project".into(),
        tier: "always_ask".into(),
        outcome: "denied".into(),
        repeat_count: 1,
        breaker_open: false,
        grant: "grant `adj-1` (write) for this session".into(),
    });

    let retained = hub.retained();
    let mut scrub = letibot_sessionlog::StoredProjection::of(retained.iter());
    let kept = retained
        .iter()
        .filter_map(|e| scrub.keep(e))
        .filter(|e| matches!(e.event, SessionEvent::DenialRaised { .. }))
        .count();
    assert_eq!(
        kept, 1,
        "scrubbing a denial puts back the hole §4b was written against, one head \
         later: somebody who joined after the refusal sees only a dead task"
    );
}
