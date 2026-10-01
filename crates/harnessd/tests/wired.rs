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
use letibot_harnessd::harness::ForkTail;
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
    // **These tests assert what an UNCONFIGURED session seats**, so the fixture
    // supplies nothing to configure. `Config::for_this_box` reads the operator's
    // own `~/.config/letibot` — `providers.toml` for a search key,
    // `permission.json` for the preapproved list — and without this the answer
    // depends on whose machine is running them. Caught 2026-09-15: a Brave key in
    // the operator's `providers.toml` seated `web_search` and turned the
    // default-tool-set assertion below into a claim about that laptop.
    //
    // Set as FIELDS rather than by pinning `$XDG_CONFIG_HOME`: these tests run in
    // parallel threads, and `set_var` beside a running thread is undefined
    // behaviour that this edition aborts the process for — measured, after all ten
    // had already passed.
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.seat = seat;
    if let Ok(g) = std::env::var("LETIBOT_VOCAB_GGUF") {
        cfg.vocab_gguf = g.into();
    }
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    // **The operator's per-project mode is not this test's business.**
    //
    // `Harness::open` applies the project store's row over `cfg.mode` (D13), and
    // the row for this repository is whatever the operator last set with `/mode`.
    // The moment he set `automode`, eight of these went red demanding an
    // authorisation oracle — a correct refusal about a mode the test never asked
    // for. The third time a test here has been decided by this laptop's
    // configuration rather than by its own fixture; `wired`'s job is to assert
    // what a session seats, so it supplies the conditions it is asserting about.
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let cfg = config(Seat::default());
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open(&p, cfg, hub).expect("the default session opens with no flags");
    let w = h.wiring();

    assert_eq!(w.role, "orchestrator");
    // `todo_write` is the session's own list, `flowy` the door to the room — a
    // door with nothing behind it in a session opened without a seat, which says
    // so and names `/flowy login`. Both were decided (2026-09-14); the next
    // addition is not a decision until it is written down here too.
    assert_eq!(
        w.seated,
        vec![
            "read",
            "grep",
            "glob",
            "ask_code",
            "ask_corpus",
            "read_spill",
            "todo_write",
            "flowy"
        ],
        "the default tool set changed; that is a re-prefill for every stored session \
         as well as a capability change"
    );
    assert!(!w.has_write_tools, "seated: {:?}", w.seated);
    assert!(!w.has_exec_tools, "seated: {:?}", w.seated);
    // `flowy` declares `Network`: the door is seated, so the session opens with a
    // gate that can name it — nothing behind it is reached without a seat.
    assert!(w.has_network_tools, "seated: {:?}", w.seated);
    assert!(!w.backend_writable);
    // The door is gated, so the session carries the head adjudicator — and says
    // that nobody is attached to it, rather than claiming one is. A read-only
    // session with no door would need none; this one has a door.
    assert!(
        w.adjudicator.contains("none attached"),
        "no head is attached and the adjudicator must say so: {}",
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let cfg = config(Seat::default());
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open(&p, cfg, hub).expect("opens");
    for absent in [
        "write",
        "edit",
        "bash",
        "job_list",
        "monitor",
        "web_search",
        "todo",
    ] {
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    cfg.grants_ro =
        vec![std::path::PathBuf::from(std::env::var("HOME").expect("a home")).join(".cargo")];
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
    assert!(
        all.contains(".cargo"),
        "the grant is not in the disclosure: {all}"
    );
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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
        scripts: &[],
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
    // §4b's requirement: the way OUT arrives with the denial, not after the task
    // has died — a refusal that can only be routed around teaches people to route
    // around refusals, and one whose remedy arrives late is that with an extra
    // step. What the way out IS depends on who refused.
    //
    // This call is refused by `boundary:normaliser` (the gate here declares no
    // surroundings, so a bare `rm` is an unresolved program), and there a grant is
    // not the remedy: the refusal happens before any adjudicator and a standing
    // permission is tested after it, so granting it changes nothing on a retry.
    // Offering `grant adj-N` there is offering a button that does nothing, which
    // teaches the same lesson this assertion exists to prevent.
    assert_eq!(by, "boundary:normaliser");
    assert!(
        grant.contains("re-issuing the command"),
        "a boundary refusal must name the remedy that exists: {grant}"
    );
    assert!(
        !grant.contains("adj-"),
        "offered a grant that the gate would ignore on the retry: {grant}"
    );

    // And where a person COULD have answered, the id is there to answer it by. A
    // `write` resolves without a declared shell, so this one reaches `NoAdjudicator`
    // itself rather than stopping at layer A.
    let args = serde_json::json!({"path": "/home/dead/Projects/letibot/notes.md"});
    let _ = gate.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c2",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(false),
        scripts: &[],
    });
    let adjudicable: Vec<(String, String)> = hub
        .retained()
        .into_iter()
        .filter_map(|e| match e.event {
            SessionEvent::DenialRaised { by, grant, .. } if !by.starts_with("boundary:") => {
                Some((by, grant))
            }
            _ => None,
        })
        .collect();
    let (by, grant) = adjudicable
        .last()
        .expect("a call nobody could answer is still a refusal somebody can grant");
    assert_eq!(
        by, "none",
        "nobody was attached, and that is who refused it"
    );
    assert!(
        grant.contains("adj-"),
        "the operator must be able to grant it by name: {grant}"
    );
}

/// The denial is **durable**: a head attaching after the refusal replays it, rather
/// than joining a session where a task stopped for no visible reason.
#[test]
fn a_denial_is_replayed_to_a_head_that_was_not_there() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
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

// ---------------------------------------------------------------------------
// The operator's sequence, 2026-09-20: "switched leticl session to allow-all,
// pressed y, got a warning, still asked about grep".

/// A leticode session, as the daemon opens one: unconfined, `bash` seated, the
/// point it actually starts at.
fn leticode_cfg(session: &str) -> Config {
    let mut cfg = config(Seat::Leticode);
    cfg.session_id = session.into();
    cfg.allow_bash = true;
    cfg.mode = letibot_tools::mode::Mode::AUTO_EDITS;
    // automode-edits needs an oracle to open; the endpoint is never contacted
    // here (reachability is deliberately not probed at open).
    cfg.oracle = Some(letibot_turn::Endpoint::parse("127.0.0.1:8080").expect("endpoint"));
    cfg
}

#[test]
fn consenting_to_allow_all_admits_a_shell_command_unasked() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let cfg = leticode_cfg("allow-all-exec");
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
        .expect("a leticode session opens at automode-edits");

    // The move the head sends after the operator presses `y`.
    let said = h
        .set_mode_consented(letibot_tools::mode::Mode::ALLOW_ALL, true)
        .expect("consent moves the session");
    eprintln!("  set_mode_consented -> {said}");
    assert!(
        said.contains("this box") || said.contains("confirmation"),
        "the move must say it stands on the confirmation: {said}"
    );
}

// ---------------------------------------------------------------------------
// The custom base prompt, and what a compaction does with it.

/// **`--system` reaches the rendered prefix**, so a daemon started with the
/// operator's own base prompt is speaking it.
#[test]
fn a_custom_system_prompt_is_what_the_session_opens_under() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let short = "You judge.";
    let long = "You judge. ".repeat(200);

    let open_with = |system: &str| -> usize {
        let mut cfg = config(Seat::Orchestrator);
        cfg.system = system.to_string();
        cfg.session_id = format!("sysprompt-{}", system.len());
        let p = parts(&cfg);
        let hub = Hub::new(&cfg.session_id);
        let h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
            .expect("the session opens");
        h.ledger_len()
    };

    let a = open_with(short);
    let b = open_with(&long);
    assert!(
        b > a + 100,
        "a longer --system must render into a longer prefix: {a} vs {b}"
    );
}

/// **A compaction forks onto the prefix, so the base prompt comes back in front
/// of the summary.**
///
/// The operator, asking for a daemon with a custom base prompt: *"This also
/// means that when we compact we must prepend our base prompt to the summary
/// produced by the model."* It already does — `fork_to_summary` opens the new
/// transcript under this session's own `StablePrefix` — and this is that, pinned,
/// without needing a model: the fork is given a summary and asked to build the
/// new base from it.
#[test]
fn a_fork_puts_the_base_prompt_in_front_of_the_summary() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let system = "SYSTEM-MARKER: you are the guard. ".repeat(40);
    let dir = std::env::temp_dir().join(format!("letibot-fork-prefix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = dir.join("s.db");

    let mut cfg = config(Seat::Orchestrator);
    cfg.system = system.clone();
    cfg.store = Some(store.clone());
    cfg.session_id = "fork-prefix".into();
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
        .expect("the session opens");

    let prefix_tokens = h.ledger_len();
    assert!(prefix_tokens > 100, "the marker prompt should be sizeable");

    let outcome = letibot_turn::CompactionOutcome {
        turn_id: "t#summary".into(),
        summary: "decided: nothing. open: nothing.".into(),
        tool_calls: 0,
        truncated: false,
        cached_tokens: 0,
        reusable: 0,
        generated_tokens: 0,
    };
    let fork = h
        .fork_to_summary(&outcome, None, None, ForkTail::NONE)
        .expect("the fork");

    // The new base is the prefix PLUS the summary — not the summary alone. If
    // the base prompt had been dropped the base would be a couple of dozen
    // tokens; it is the prefix again, with the summary after it.
    assert!(
        fork.base_tokens > prefix_tokens,
        "the base lost the prompt: base {} vs prefix {prefix_tokens}",
        fork.base_tokens
    );
    // And the summary really is in there, as the first item of the new
    // transcript, after the prefix the ledger already carries.
    let first = h.items().first().expect("the new base has an item");
    match first {
        letibot_transcript::TranscriptItem::System { text, .. } => {
            assert!(text.contains("decided: nothing"), "{text}");
        }
        other => panic!("the first item is not the summary: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// **Re-seating without summarising runs no summary turn and keeps the history.**
///
/// The operator: *"is there a way to reseat without summarizing? … like reingest
/// full context"*. A tool's schema lives in the stable prefix, so a schema change
/// needs a fork; paying for that fork with a summary turn is right when the point
/// is to shrink and pure loss when the point is only to change message zero.
///
/// No model runs here, and that IS the assertion: `reseat` could not be tested
/// this way at all, because it has to ask one for a summary.
#[test]
fn reingest_writes_no_summary_and_says_so_in_the_note() {
    let Some(_) = letibot_tokencore::apparatus::present_gguf() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("letibot-reingest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    let mut cfg = config(Seat::Orchestrator);
    cfg.store = Some(dir.join("s.db"));
    cfg.session_id = "reingest".into();
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let mut h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
        .expect("the session opens");

    // Nothing to re-seat onto yet: the conversation already speaks the seated
    // prompt, and saying so is the right refusal.
    let Err(err) = h.reingest() else {
        panic!("re-seating a conversation already on the seated prompt should refuse");
    };
    assert!(err.to_string().contains("already carries"), "{err}");

    // Change message zero — which is exactly what seating a new tool does to the
    // prefix — and now there is.
    h.config_mut().system = "a different base prompt entirely".into();
    let r = h.reingest().expect("reingest");

    assert!(r.summary_turn.summary.is_empty(), "a summary turn ran");
    assert_eq!(r.summary_turn.generated_tokens, 0, "a model was asked");
    // The note says what happened, and does not claim a summary the reader
    // cannot see.
    match h.items().first().expect("the new base has an item") {
        letibot_transcript::TranscriptItem::System { text, .. } => {
            assert!(text.contains("re-seated onto a new prompt"), "{text}");
            assert!(text.contains("Nothing was summarised"), "{text}");
            assert!(
                !text.contains("replaced by the summary"),
                "it claims a compaction: {text}"
            );
        }
        other => panic!("the note is not first: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
