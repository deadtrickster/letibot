//! What a session can actually reach, and what it says about it. Everything under test here
//! was built, tested and unreachable before this file existed

use letibot_harnessd::config::{Config, Seat};
use letibot_harnessd::harness::ForkTail;
use letibot_harnessd::{Dialect, Harness, Parts};
use letibot_sessionlog::hub::Hub;
use letibot_tools::Adjudicator;

mod support;

fn config(seat: Seat) -> Config {
    // The workspace is this repository: `HostBackend::new` canonicalises its root
    // so it has to be a directory that exists, and a tree with files in it is what
    // a seated `read` would be pointed at anyway.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf();
    let mut cfg = Config::for_this_box(repo);
    // These tests assert what an UNCONFIGURED session seats, so the fixture supplies nothing
    // to configure
    cfg.web_search = None;
    cfg.permission = Vec::new();
    cfg.dialect = Dialect::Qwen;
    cfg.seat = seat;
    // `Config::for_this_box` carries no vocabulary default any more (a daemon on the byte vocabulary
    // needs none), so a harness built here is handed the operator's `LETIBOT_VOCAB_GGUF` if it is set
    // else the box's own GGUF - the path this file's `vocab_gguf` gate consults.
    cfg.vocab_gguf = support::vocab::gguf();
    cfg
}

fn parts(cfg: &Config) -> Parts {
    let p = Parts::load(cfg).expect(
        "the vocabulary must load; set LETIBOT_VOCAB_GGUF if this box is not the one \
         this repository is developed on",
    );
    // The operator's per-project mode is not this test's business.
    *p.mode_store.write().unwrap() = letibot_harnessd::modes::ModeStore::default();
    p
}

/// The coder seat for a test about the SEAT and not about its boundary. `coder` is one of
/// the two confined seats, so `Harness::open` refuses it where no boundary can be built -
/// macOS, where there are no namespaces to enter.
fn seat_config(seat: Seat) -> Config {
    let mut cfg = config(seat);
    cfg.unconfined = cfg!(target_os = "macos");
    cfg
}

/// An adjudicator that would admit anything, and never gets the chance to. It exists so a
/// *seating* test does not need a human at a terminal
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

/// The invocation that passes no flag gets what it got before. This is the rule the whole
/// strand is under, and the way it regresses is not a decision - it is somebody adding a
/// tool to the default role because the role now has somewhere to put it.
#[test]
fn the_default_session_is_read_only_and_seats_exactly_what_it_did() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let cfg = config(Seat::default());
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open(&p, cfg, hub).expect("the default session opens with no flags");
    let w = h.wiring();

    assert_eq!(w.role, "orchestrator");
    // `todo_write` is the session's own list and `flowy` the door to the room (without a seat it says so and names `/flowy login`).
    // `recall` arrived with the compacted base's map: it is a re-prefill for every stored session and a capability change, so it is asserted here.
    assert_eq!(
        w.seated,
        vec![
            "read",
            "grep",
            "glob",
            "ask_code",
            "ask_corpus",
            "read_spill",
            "recall",
            "todo_write",
            "room",
            "flowy"
        ],
        "the default tool set changed; that is a re-prefill for every stored session \
         as well as a capability change"
    );
    assert!(!w.has_write_tools, "seated: {:?}", w.seated);
    assert!(!w.has_exec_tools, "seated: {:?}", w.seated);
    // `flowy` declares `Network`: the door is seated, so the session opens with a gate that can name
    // it - nothing behind it is reached without a seat.
    assert!(w.has_network_tools, "seated: {:?}", w.seated);
    assert!(!w.backend_writable);
    // The door is gated, so the session carries the head adjudicator - and says that nobody is
    // attached to it rather than claiming one is.
    assert!(
        w.adjudicator.contains("none attached"),
        "no head is attached and the adjudicator must say so: {}",
        w.adjudicator
    );

    // The one thing that IS new for a default session, and it is instrumentation rather than
    // capability: the encoder measures what turns did.
    assert!(
        w.intent_encoder,
        "the intent encoder is attached for every session; without it every \
         completion is `NoEncoder` and the reason is ours"
    );
}

/// A layer's tool is seated when a layer IN FORCE declares it, and not otherwise. The other
/// half of the test above: the same seat, the same flags, one `plugin.toml` in the project
/// and the tool is in the prompt; take the file away and it is not.
#[test]
fn a_layer_in_force_seats_its_tool_and_no_layer_seats_none() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // A project of its own: this test is about a layer on disk, and the repository it runs in may
    // have plugins of its own (a `.letibot/plugins` is a thing a session's project has).
    let project = std::env::temp_dir().join(format!(
        "letibot-wired-layer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&project).expect("a project dir");

    let seated_with = |session: &str| {
        let mut cfg = config(Seat::default());
        cfg.workspace = project.clone();
        cfg.session_id = session.to_string();
        let p = parts(&cfg);
        let hub = Hub::new(&cfg.session_id);
        let h = Harness::open(&p, cfg, hub).expect("the session opens");

        h.wiring().clone()
    };

    // 1. No layer: the default set, exactly - which is what the test above asserts tool by tool.
    let none = seated_with("s-wired-no-layer");
    assert!(
        !none.seated.iter().any(|t| t == "claude_transcript"),
        "no layer declares it, so nothing seats it: {:?}",
        none.seated
    );

    // 2. The binary's own manifest, installed by the project. One file, no module.
    let builtin = letibot_builtins::table()
        .iter()
        .find(|b| b.name == "claude-transcript")
        .expect("the binary carries the transcript layer");
    let layer = project
        .join(".letibot")
        .join("plugins")
        .join("claude-transcript");
    std::fs::create_dir_all(&layer).expect("the layer dir");
    std::fs::write(layer.join("plugin.toml"), builtin.manifest).expect("the manifest");

    let with = seated_with("s-wired-with-layer");
    assert!(
        with.seated.iter().any(|t| t == "claude_transcript"),
        "the project declares it, so the model is offered it: {:?}",
        with.seated
    );
    // And it is the only difference - a layer's tool adds a tool and changes nothing else
    // about what the seat can reach. It reads a tree outside the workspace, which is `Read`
    // and not a write or a shell
    assert_eq!(
        with.seated.len(),
        none.seated.len() + 1,
        "one layer, one tool: {:?} vs {:?}",
        with.seated,
        none.seated
    );
    assert!(!with.has_write_tools, "seated: {:?}", with.seated);
    assert!(!with.has_exec_tools, "seated: {:?}", with.seated);

    std::fs::remove_dir_all(&project).ok();
}

/// The registry is a superset and the role decides what the prompt carries. The failure
/// this rules out is the one §8.4 names: a build that registers ten tools and quietly seats
/// eight.
#[test]
fn a_registered_tool_the_role_does_not_name_is_not_in_the_prompt() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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

/// A write seat with nobody to decide does not open. The inverse of a gate that says
/// allowed because nothing is wired, and it costs the same way
#[test]
fn a_write_seat_with_no_adjudicator_refuses_to_start_and_says_what_to_attach() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let e = open(
        seat_config(Seat::Coder),
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
    // And it must say why refusing to *start* is different from refusing to *run*
    // because "it fails closed already" is the objection this answers.
    assert!(e.contains("not_run"), "{e}");
}

/// The same seat opens once somebody is attached, and the wiring says all three
/// seams are on - not because a constructor was called, but because the gate and
/// the ledger were asked.
#[test]
fn a_write_seat_with_an_adjudicator_opens_fully_wired() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let cfg = seat_config(Seat::Coder);
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

/// The runner seats eight of its nine tools, and the ninth needs its own flag. If this box
/// cannot build a confined backend the test says so and stops rather than passing
#[test]
fn the_runner_role_does_not_seat_bash_without_its_own_flag() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
            return letibot_tokencore::apparatus::absent(&format!(
                "a boundary this box can build (the refusal is the correct outcome, not an \
                 unconfined backend): {e}"
            ));
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

/// The same for `coder`, which is the seat an operator actually sits in. The daemon strips
/// `bash` back off unless `--bash` is passed, exactly as it does for the runner. Two seats
/// one rule, asserted twice because the strip is written per-seat
#[test]
fn the_coder_role_does_not_seat_bash_without_its_own_flag_and_grants_reach_the_view() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
            return letibot_tokencore::apparatus::absent(&format!(
                "a boundary this box can build: {e}"
            ));
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
    // with its consequence - read-only in the view still means readable into the
    // transcript, and a disclosure that omits that is the decision without its cost.
    let all = h2
        .config()
        .disclosures(w)
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    // A grant is a rule of a boundary, so there is nothing for the view to name when there is
    // no view.
    if w.backend.contains("NOT CONFINED") {
        return letibot_tokencore::apparatus::absent(&format!(
            "a boundary for this checkout ({}), so there is no view for a grant to be in",
            w.backend
        ));
    }
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

/// D9's plan mode, and the two tools that make a plan a plan. The interesting assertion is
/// the negative one
#[test]
fn the_planner_seat_can_write_its_plan_and_not_the_work() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
/// pretending - which is a *different* thing from not being seated.
#[test]
fn the_researcher_seat_carries_seams_that_refuse_by_name() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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

/// §4b, end to end through the parts that will actually carry it. The chain the requirement
/// exists for
#[test]
fn a_refusal_reaches_the_operators_log_at_the_moment_it_is_decided() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
    // `not_run` and `denied` must never collapse. Nobody decided here, and
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
    // §4b's requirement: the way OUT arrives with the denial, not after the task has died - a
    // refusal that can only be routed around teaches people to route around refusals. What the
    // way out IS depends on who refused.
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

/// Who holds the decision is a fact about a screen, and the model plans on it, so the sentence
/// it is given comes from the heads the daemon actually fanned the refusal out to.
#[test]
fn a_refusal_says_a_person_was_told_only_when_a_head_is_attached() {
    use letibot_harnessd::harness::HubDenials;
    use letibot_sessionlog::Caps;
    use letibot_tools::{Access, AdjudicatedGate, Gate, GateCall, GateDecision, NoAdjudicator};

    // A corpus sink is what makes `/gate recent` a true answer: the row the refusal names is
    // written where `/gate` reads it, and the sentence may say so.
    let corpus = || {
        std::sync::Arc::new(letibot_tools::RecordingCorpusSink::default())
            as std::sync::Arc<dyn letibot_tools::CorpusSink>
    };
    let hub = Hub::new("s-told");
    let mut gate = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_identity("s-told", "dead")
        .with_corpus_sink(corpus())
        .with_denial_sink(Box::new(HubDenials::new(hub.clone())));
    let args = serde_json::json!({"path": "/home/dead/Projects/letibot/notes.md"});
    let told = |d: GateDecision| match d {
        GateDecision::Refuse { tell, .. } => tell,
        other => panic!("the call must be refused: {other:?}"),
    };

    // Nobody at the keyboard: the decision is in the record, and the model must not be told
    // that a person knows about it.
    let first = told(gate.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(false),
        scripts: &[],
    }));
    assert!(!first.contains("The operator has been told"), "{first}");
    assert!(first.contains("Nobody has been told"), "{first}");
    // And it says plainly where the decision is, which is what answers "what decision" later.
    assert!(
        first.contains("`adj-s-told-0001`") && first.contains("/gate recent"),
        "the row a person reads it in: {first}"
    );
    assert!(
        first.contains("Nothing is waiting for an answer"),
        "no ask of this refusal is open: {first}"
    );

    // A head attaches, so the next refusal is on its screen and the model is told so.
    hub.attach("tui", "dead", Caps::default(), 0);
    let second = told(gate.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c2",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(false),
        scripts: &[],
    }));
    assert!(second.contains("The operator has been told"), "{second}");
    assert!(!second.contains("Nobody has been told"), "{second}");

    // A connector that attached to WATCH - a bridge with `can_decide: false` - is not a person
    // who may answer, so it is not a screen the model may be told is holding the decision.
    let watching = Hub::new("s-watched");
    watching.attach(
        "bridge",
        "phone",
        Caps {
            can_decide: false,
            ..Caps::default()
        },
        0,
    );
    let mut gate = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_identity("s-watched", "dead")
        .with_denial_sink(Box::new(HubDenials::new(watching.clone())));
    let watched = told(gate.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(false),
        scripts: &[],
    }));
    assert!(!watched.contains("The operator has been told"), "{watched}");
    assert!(watched.contains("Nobody has been told"), "{watched}");

    // And the console: the person who answered the brief HAS seen the decision, and there is no
    // head at all to count. Telling the model nobody was told talks it out of asking again.
    let console = Hub::new("s-console");
    let mut gate = AdjudicatedGate::new(Box::new(letibot_tools::ConsoleAdjudicator::new(
        "dead",
        Box::new(std::io::Cursor::new(b"deny\n".to_vec())),
        Box::new(Vec::new()),
    )))
    .with_identity("s-console", "dead")
    .with_denial_sink(Box::new(HubDenials::new(console.clone())));
    let answered = told(gate.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(false),
        scripts: &[],
    }));
    assert!(
        answered.contains("who decided: human:dead"),
        "the console operator decided: {answered}"
    );
    assert!(
        answered.contains("The operator has been told"),
        "{answered}"
    );
    assert!(!answered.contains("Nobody has been told"), "{answered}");

    // And the case the entry was written against: no head now, but the ask is still open, so a
    // head that attaches is handed it. The model may be told an answer given then is read, and
    // must not be told nothing is waiting.
    let holding = Hub::new("s-holding");
    holding.publish(letibot_sessionlog::testing::requested(
        "adj-s-holding-0001",
        "restart the daemon",
    ));
    let mut gate = AdjudicatedGate::new(Box::new(NoAdjudicator))
        .with_identity("s-holding", "dead")
        .with_corpus_sink(corpus())
        .with_denial_sink(Box::new(HubDenials::new(holding.clone())));
    let held = told(gate.admit(&GateCall {
        name: "write",
        access: Access::Write,
        args: &args,
        turn_id: "t1",
        call_id: "c1",
        workspace: "/home/dead/Projects/letibot",
        target_exists: Some(false),
        scripts: &[],
    }));
    assert!(!held.contains("The operator has been told"), "{held}");
    assert!(held.contains("no operator screen is attached"), "{held}");
    assert!(
        held.contains("a head that attaches is handed it"),
        "the ask a head attaching is shown: {held}"
    );
    assert!(!held.contains("Nothing is waiting for an answer"), "{held}");
}

/// The denial is durable: a head attaching after the refusal replays it, rather
/// than joining a session where a task stopped for no visible reason.
#[test]
fn a_denial_is_replayed_to_a_head_that_was_not_there() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
// A session switched to allow-all, and what it still asks about
// ---------------------------------------------------------------------------

/// A leticode session, as the daemon opens one: unconfined, `bash` seated, the point it actually
/// starts at.
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
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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
// `ask_user_question`: the seat can ask, the head answers, the model is told.

/// A leticode session can ask the person a question, and the answer reaches the model. End
/// to end and not a seating assertion. The tool is invoked through the session's own
/// runtime with the session's own registry and its own gate
#[test]
fn a_leticode_session_asks_the_person_and_the_answer_reaches_the_model() {
    use letibot_sessionlog::protocol::Caps;
    use letibot_tools::events::NullToolSink;

    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let cfg = leticode_cfg("ask-choice");
    let p = parts(&cfg);
    let hub = Hub::new(&cfg.session_id);
    let h = Harness::open_with(&p, cfg, hub.clone(), None, None)
        .expect("a leticode session opens with the head adjudicator");

    // The seat carries it. Read off the resolved wiring, which is what the prompt
    // is built from - not off the role's name.
    let w = h.wiring();
    assert!(
        w.seated.iter().any(|t| t == "ask_user_question"),
        "the leticode seat does not carry `ask_user_question`: {:?}",
        w.seated
    );

    // A head that can decide, playing itself on its own thread.
    let head = hub.attach("tui", "deadtrickster", Caps::default(), 0);
    let hub2 = hub.clone();
    let head_id = head.head_id.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let answerer = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            let open = hub2.snapshot().open_decisions;
            if let Some(d) = open.first() {
                // The card a person would be looking at: a question, the model's own
                // choices, and the one line saying what it is stuck on.
                assert_eq!(d.kind, "question", "the head was shown a permission");
                assert_eq!(d.choices, vec!["postgres", "sqlite"]);
                hub2.submit(
                    &head_id,
                    "c1",
                    0,
                    letibot_sessionlog::hub::CommandKind::Answer {
                        req_id: d.req_id.clone(),
                        reply: letibot_sessionlog::hub::Reply::Question(
                            letibot_sessionlog::question::QuestionAnswer::choosing(1)
                                .with_note("only for the CUDA box"),
                        ),
                    },
                );
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    });

    // The call, as the engine would make it.
    let rt = h.runtime_handle();
    let call = letibot_transcript::ToolCall {
        id: "call_1".into(),
        name: "ask_user_question".into(),
        arguments: serde_json::json!({
            "question": "which database should the migration target?",
            "options": ["postgres", "sqlite"],
            "because": "the two need different migration files",
        })
        .to_string(),
    };
    let mut sink = NullToolSink;
    let r = rt.invoke("t1", &call, &mut sink);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        answerer.join().unwrap(),
        "the question never reached the head: {r:?}"
    );

    // What the model gets: the choice, the note beside it, and the person's name
    // on the whole of it. A note that arrived as a separate sentence would be a
    // qualification that lost the choice it qualifies.
    assert_eq!(r.outcome, letibot_transcript::ToolOutcome::Ok, "{r:?}");
    let out = r.render();
    assert!(out.contains("chose option 1: sqlite"), "{out}");
    assert!(
        out.contains("with the note: only for the CUDA box"),
        "{out}"
    );
    assert!(
        out.contains("deadtrickster chose option 1: sqlite"),
        "the answer is not attributed to the person beside the choice: {out}"
    );
}

// ---------------------------------------------------------------------------
// The custom base prompt, and what a compaction does with it.

/// `--system` reaches the rendered prefix, so a daemon started with the
/// operator's own base prompt is speaking it.
#[test]
fn a_custom_system_prompt_is_what_the_session_opens_under() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
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

/// A compaction forks onto the prefix, so the base prompt comes back in front of the
/// summary. `fork_to_summary` opens the new transcript under this session's own
/// `StablePrefix`, and this pins that without needing a model
#[test]
fn a_fork_puts_the_base_prompt_in_front_of_the_summary() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let system = "SYSTEM-MARKER: you are the guard. ".repeat(40);
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-fork-prefix-{}", std::process::id()));
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
        budget: None,
        fidelity: None,
    };
    let fork = h
        .fork_to_summary(&outcome, None, None, ForkTail::NONE)
        .expect("the fork");

    // The new base is the prefix PLUS the summary - not the summary alone. If
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

/// Re-seating without summarising runs no summary turn and keeps the history. A tool's
/// schema lives in the stable prefix, so a schema change needs a fork
#[test]
fn reingest_writes_no_summary_and_says_so_in_the_note() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-reingest-{}", std::process::id()));
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

    // Change message zero - which is exactly what seating a new tool does to the
    // prefix - and now there is.
    h.config_mut().system = "a different base prompt entirely".into();
    let r = h.reingest().expect("reingest");

    assert!(r.summary_turn.summary.is_empty(), "a summary turn ran");
    assert_eq!(r.summary_turn.generated_tokens, 0, "a model was asked");
    // The note says what happened, and does not claim a summary the reader
    // cannot see.
    match h.items().first().expect("the new base has an item") {
        letibot_transcript::TranscriptItem::System { text, .. } => {
            assert!(text.contains("System prompt and tools updated"), "{text}");
            assert!(text.contains("Nothing was summarised"), "{text}");
            assert!(
                !text.contains("transcript"),
                "no transcript id (R93.E7): {text}"
            );
            assert!(
                !text.contains("replaced by the summary"),
                "it claims a compaction: {text}"
            );
        }
        other => panic!("the note is not first: {other:?}"),
    }
    // And the harness now speaks the prompt it re-seated onto. `reingest` forked onto the new
    // prefix and left `self.prefix` on the old one, so a metered turn went on getting the OLD schemas
    // after a `/reseat` that reported them rebuilt. `reseat_target` compares against `self.prefix`.
    let Err(err) = h.reingest() else {
        panic!("a second re-seat found something to do: the harness never adopted the first");
    };
    assert!(err.to_string().contains("already carries"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The base prompt is composed for the tools the seat actually holds
// ---------------------------------------------------------------------------

/// The facts the prompt is composed from are the seated registry's, and the prompt
/// names no tool outside them - for each seat, with and without `--bash`. The unit half
/// over the roles' own lists, is `config::tests::the_prompt_names_no_tool_the_seat_lacks`.
#[test]
fn seat_facts_are_the_seated_tools() {
    let Some(_) = letibot_harnessd::vocab_gguf() else {
        return;
    };
    // Every name any seat can hold, to tell a tool name from an ordinary backticked word.
    let mut every: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for seat in [
        Seat::Leticode,
        Seat::Coder,
        Seat::Gatekeeper,
        Seat::NotesKeeper,
        Seat::Orchestrator,
    ] {
        for bash in [true, false] {
            let mut cfg = seat_config(seat);
            cfg.allow_bash = bash;
            cfg.session_id = format!("seat-facts-{}-{bash}", seat.as_str());
            cfg.seat_prompt = Some(letibot_harnessd::config::SeatPrompt::default());
            let p = parts(&cfg);
            let hub = Hub::new(&cfg.session_id);
            let h = Harness::open_with(&p, cfg, hub, Some(Box::new(Attached)), None)
                .unwrap_or_else(|e| panic!("{} opens: {e}", seat.as_str()));
            let facts = h.config().seat_facts.clone().expect("set at open");
            assert!(facts.has("read"), "{facts:?}");
            every.extend(facts.tools.iter().cloned());
            // The standing notes are the operator's material, not the prompt's claims.
            let system = letibot_harnessd::standing_notes::replace(&h.config().system, None);
            let lacks: Vec<&str> = system
                .split('`')
                .skip(1)
                .step_by(2)
                .filter(|w| every.contains(*w) && !facts.has(w))
                .collect();
            assert!(
                lacks.is_empty(),
                "{} (bash {bash}) is told about {lacks:?}:\n{system}",
                seat.as_str()
            );
            assert!(!system.contains("read-only tools"), "{system}");
        }
    }
}
