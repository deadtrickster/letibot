//! `allow-all` is really allow-all: no boundary, no fence on the file tools, and no card - for the
//! session and for its children. Plus the point a project nobody configured opens at, and what a
//! box that cannot supply it gets.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A scratch directory that removes itself.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = letibot_sessionlog::testing::scratch_root().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(p.join("ws")).unwrap();
        Scratch(p)
    }

    fn ws(&self) -> std::path::PathBuf {
        self.0.join("ws")
    }

    fn db(&self) -> std::path::PathBuf {
        self.0.join("sessions.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `coder` on this box's byte vocabulary, with nothing set beyond the seat: the substrate is what
/// the point decides, which is the whole of what the tests below are about.
fn config(dir: &Scratch, id: &str) -> Config {
    let mut cfg = Config::for_this_box(dir.ws());
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.db());
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    cfg.seat = crate::config::Seat::Coder;
    // A search key on the box seats `web_search`, and a gated tool with nobody behind the gate
    // refuses the open.
    cfg.web_search = None;
    cfg
}

/// [`config`] on the whole-host substrate WITHOUT a point that admits everything: what this box can
/// actually build, because a `coder` asks for a confined backend and `bwrap` has no user namespace
/// here to make one in. It is what the fence's control and the mid-session move are given.
fn on_the_whole_host(dir: &Scratch, id: &str) -> Config {
    let mut cfg = config(dir, id);
    cfg.unconfined = true;
    cfg.allow_bash = false;
    cfg
}

/// A box as it comes: the default seat, nothing configured, and the point a fresh install has.
/// Enough to answer what the default point is and what a box that cannot supply it gets.
fn bare(dir: &Scratch, id: &str) -> Config {
    let mut cfg = Config::for_this_box(dir.ws());
    // The box's own default, which `for_this_box` does not give a fixture: this one is about it.
    cfg.mode = letibot_tools::mode::UNSEEN_PROJECT;
    cfg.dialect = crate::Dialect::Qwen;
    cfg.store = Some(dir.db());
    cfg.session_id = id.into();
    cfg.vocab_gguf = None;
    cfg.web_search = None;
    cfg
}

/// How many calls were put to a person. The whole of "no card" is that this stays zero: the
/// adjudicator answers `unavailable`, so a card is also a failed call and not only a count.
type Cards = Arc<AtomicUsize>;

fn open(cfg: Config) -> (Harness, Cards) {
    let cards: Cards = Arc::new(AtomicUsize::new(0));
    let seen = cards.clone();
    let adjudicator = letibot_tools::adjudicate::AskAdjudicator::new("human:test", move |req| {
        seen.fetch_add(1, Ordering::Relaxed);
        Some(
            letibot_tools::adjudicate::AdjudicationDecision::unavailable(
                req,
                "human:test",
                "this fixture answers no card",
            ),
        )
    });
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let id = cfg.session_id.clone();
    let h = Harness::open_with(
        &parts,
        cfg,
        Hub::new(&id),
        Some(Box::new(adjudicator)),
        None,
    )
    .expect("the session opens");
    (h, cards)
}

/// One tool call, through the runtime the turn loop uses.
fn call(h: &Harness, name: &str, args: serde_json::Value) -> letibot_tools::ToolResult {
    let call = ToolCall {
        id: "c1".into(),
        name: name.into(),
        arguments: args.to_string(),
    };
    h.runtime
        .invoke("t1", &call, &mut letibot_tools::NullToolSink)
}

fn wrote(h: &Harness, path: &std::path::Path) -> letibot_tools::ToolResult {
    call(
        h,
        "write",
        serde_json::json!({"path": path.display().to_string(), "content": "hello"}),
    )
}

/// The whole of piece 1: `allow-all` builds no boundary, and a file tool reaches a path outside the
/// project with nobody asked.
#[test]
fn allow_all_opens_with_no_boundary_and_writes_outside_the_project() {
    let dir = Scratch::new("allow-all-unbounded");
    let mut cfg = config(&dir, "s-allow-all");
    cfg.mode = letibot_tools::mode::Mode::ALLOW_ALL_HERE;
    let (h, cards) = open(cfg);

    assert!(
        h.config().unconfined,
        "a point that admits everything runs over the whole host"
    );
    assert!(
        h.runtime.backend.boundary().is_none(),
        "allow-all drew a boundary: {:?}",
        h.runtime.backend.boundary().map(|b| b.mechanism)
    );
    // Nothing claims a confinement, which is what the banner and the model read.
    assert!(
        !h.runtime.backend.describe().contains("CONFINED"),
        "{}",
        h.runtime.backend.describe()
    );

    let outside = dir.0.join("outside.txt");
    let write = wrote(&h, &outside);
    assert_eq!(
        write.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "a write outside the project was refused: {}",
        write.payload
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "hello");
    let read = call(
        &h,
        "read",
        serde_json::json!({"path": outside.display().to_string()}),
    );
    assert_eq!(read.outcome, letibot_transcript::ToolOutcome::Ok);
    assert!(read.payload.contains("hello"), "{}", read.payload);
    assert_eq!(
        cards.load(Ordering::Relaxed),
        0,
        "a card was put to a person"
    );
}

/// The control on the same substrate: a point that does not admit everything fences the file tools
/// to the project, and the same call is refused. A child's perimeter is the tight one, so the fence
/// is what answers here.
#[test]
fn a_narrow_child_still_cannot_write_outside_the_project() {
    let dir = Scratch::new("allow-all-control");
    let mut modes = crate::modes::ModeStore::load(&dir.db());
    modes
        .set(&dir.ws(), letibot_tools::mode::Mode::WRITES_ALLOWED)
        .expect("the project's row");
    let mut cfg = on_the_whole_host(&dir, "s-parent-sub-3");
    cfg.parent_session_id = Some("s-parent".into());
    let (h, cards) = open(cfg);
    assert_eq!(h.config().mode.name, "writes allowed");
    let outside = dir.0.join("outside.txt");
    let write = wrote(&h, &outside);
    assert_ne!(
        write.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "a writes-allowed child wrote outside its project"
    );
    assert!(!outside.exists(), "and nothing was created");
    assert_eq!(
        cards.load(Ordering::Relaxed),
        0,
        "the fence answers, not a person"
    );
}

/// Piece 2: an allow-all parent's child is allow-all too - the same substrate, the same reach,
/// nobody asked. A child carries the point rather than holding a row of its own, which is what
/// `task` leaves behind.
#[test]
fn an_allow_all_parents_child_writes_outside_the_project_unasked() {
    let dir = Scratch::new("allow-all-child");
    let mut modes = crate::modes::ModeStore::load(&dir.db());
    modes
        .set(&dir.ws(), letibot_tools::mode::Mode::ALLOW_ALL)
        .expect("the project's row");
    modes
        .set_consent(&dir.ws(), "2026-11-18T00:00:00Z")
        .expect("the operator's answer");

    let mut cfg = config(&dir, "s-parent-sub-1");
    cfg.parent_session_id = Some("s-parent".into());
    let (h, cards) = open(cfg);

    assert_eq!(
        h.config().mode.name,
        letibot_tools::mode::Mode::ALLOW_ALL_HERE.name,
        "the child did not inherit the parent's point"
    );
    assert!(
        h.config().unconfined,
        "the child re-confined itself: a subagent re-seats to `coder`, which is not the fact"
    );
    assert!(
        h.runtime.backend.boundary().is_none(),
        "the child's commands were given a boundary: {:?}",
        h.runtime.backend.boundary().map(|b| b.mechanism)
    );

    let outside = dir.0.join("outside.txt");
    let write = wrote(&h, &outside);
    assert_eq!(
        write.outcome,
        letibot_transcript::ToolOutcome::Ok,
        "the child could not write outside the project: {}",
        write.payload
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "hello");
    assert_eq!(
        cards.load(Ordering::Relaxed),
        0,
        "the child's write was put to a person"
    );
}

/// A move to `allow-all` cannot rebuild the substrate the session opened with - the backend, its
/// view and its fence are built at open - so the move says so. What changes at once is what is
/// ASKED, which is the half `/mode` can deliver.
#[test]
fn a_move_to_allow_all_says_the_substrate_is_the_one_it_opened_with() {
    let dir = Scratch::new("allow-all-move");
    let mut cfg = on_the_whole_host(&dir, "s-move");
    cfg.mode = letibot_tools::mode::Mode::WRITES_ALLOWED;
    let (mut h, _cards) = open(cfg);
    let said = h
        .move_mode(letibot_tools::mode::Mode::ALLOW_ALL, true)
        .expect("the point is legal for this session");
    assert_eq!(
        h.config().mode.name,
        letibot_tools::mode::Mode::ALLOW_ALL_HERE.name
    );
    assert!(
        said.contains("keep the substrate this session opened with"),
        "the move did not say what it cannot change: {said}"
    );
}

/// A parent that starts the child narrower still gets a narrower child: the inheritance is a
/// carrying, not a widening.
#[test]
fn a_child_of_a_narrow_parent_is_narrow() {
    let dir = Scratch::new("allow-all-child-narrow");
    let mut modes = crate::modes::ModeStore::load(&dir.db());
    modes
        .set(&dir.ws(), letibot_tools::mode::Mode::READ_ONLY)
        .expect("the project's row");

    // The default seat, which needs no execution backend: the assertion is about the point, and a
    // `coder` cannot open on this box without the boundary it asks for.
    let mut cfg = bare(&dir, "s-parent-sub-2");
    cfg.parent_session_id = Some("s-parent".into());
    let (h, _cards) = open(cfg);
    assert!(
        h.config()
            .mode
            .no_wider_than(letibot_tools::mode::Mode::READ_ONLY),
        "a read-only parent's child opened at `{}`",
        h.config().mode.name
    );
    assert!(
        !h.config().unconfined,
        "a narrow child ran over the whole host"
    );
}

/// Piece 3, the working half: a project nobody configured opens at `automode` when the box can
/// supply the oracle that point stands on.
#[test]
fn an_unseen_project_opens_at_automode_on_a_box_with_an_oracle() {
    let dir = Scratch::new("auto-default");
    let mut cfg = bare(&dir, "s-auto");
    cfg.oracle = Some(letibot_turn::Endpoint::parse("127.0.0.1:9").expect("an endpoint"));
    let h = Harness::open(&Parts::load(&cfg).expect("parts"), cfg, Hub::new("s-auto"))
        .expect("a box with an oracle opens at the default point");
    assert_eq!(h.config().mode.name, "automode");
    assert!(!h.config().unconfined, "automode is not allow-all");
}

/// And the default that cannot be met: a box with no oracle opens at the nearest point it can carry
/// and says so in one line, rather than refusing to open every session in every folder that has no
/// mode row. A point somebody ASKED for is the test below.
#[test]
fn an_unseen_project_on_a_box_with_no_oracle_opens_at_the_nearest_point_and_says_so() {
    let dir = Scratch::new("auto-no-oracle");
    let cfg = bare(&dir, "s-no-oracle");
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let h = Harness::open(&parts, cfg, Hub::new("s-no-oracle"))
        .expect("a default nobody asked for must not stop the session opening");
    assert_eq!(
        h.config().mode.name,
        letibot_tools::mode::Mode::ALWAYS_ASK.name,
        "`automode` with no oracle is `always-ask`"
    );
    // One line, on the session, saying which point it is at and why - the operator reads it where
    // the banner is, not in a log nobody opened.
    let said = h
        .open_notes()
        .iter()
        .find(|n| n.contains("automode"))
        .unwrap_or_else(|| panic!("nothing said about the fallback: {:?}", h.open_notes()));
    assert!(said.contains("always-ask"), "{said}");
    assert!(said.contains("oracle"), "{said}");
    assert!(said.contains("/mode automode"), "{said}");
}

/// The refusing half: a point somebody ASKED for with no oracle to stand on refuses by name rather
/// than being quietly replaced, and the sentence says what to attach. This is `/mode automode`, the
/// operator's own word.
#[test]
fn an_explicit_automode_on_a_box_with_no_oracle_still_refuses_by_name() {
    let dir = Scratch::new("explicit-no-oracle");
    let cfg = bare(&dir, "s-explicit");
    let mut cfg = cfg;
    cfg.mode = letibot_tools::mode::Mode::ALWAYS_ASK;
    let parts = Parts::load(&cfg).expect("the byte vocabulary loads");
    let mut h = Harness::open(&parts, cfg, Hub::new("s-explicit")).expect("always-ask opens");
    let said = h
        .set_mode(letibot_tools::mode::Mode::AUTO)
        .expect_err("`/mode automode` has no oracle to stand on");
    assert!(said.contains("authorisation oracle"), "{said}");
    assert!(
        said.contains("[gatekeeper]"),
        "and how to attach it: {said}"
    );
    assert!(
        said.contains("refuses rather than opening at a weaker mode"),
        "{said}"
    );
    assert_eq!(
        h.config().mode.name,
        letibot_tools::mode::Mode::ALWAYS_ASK.name,
        "a refused move leaves the session where it was"
    );
}

/// `--mode allow-all` is the operator's own word, given on the command line instead of on a card: it
/// opens the same point `/mode allow-all` answered yes does, on a box with no confinement to build.
/// The alternative was a daemon that refuses to start on the operator's own machine.
#[test]
fn the_flag_spelling_of_allow_all_opens_a_host_session_with_a_shell() {
    let dir = Scratch::new("allow-all-flag");
    let mut cfg = config(&dir, "s-allow-all-flag");
    cfg.allow_bash = true;
    // The name `parse` gives from `--mode allow-all`, and the one a stale row holds.
    cfg.mode = letibot_tools::mode::Mode::ALLOW_ALL;
    let (h, _cards) = open(cfg);

    assert_eq!(
        h.config().mode.name,
        letibot_tools::mode::Mode::ALLOW_ALL_HERE.name,
        "the flag's `allow-all` was not taken as the operator's word"
    );
    assert_eq!(
        h.config().mode.boundary,
        letibot_tools::mode::Boundary::OperatorConsented,
        "and the box, not a VM, is what that word stands for"
    );
    assert!(
        h.runtime.backend.boundary().is_none(),
        "a boundary was built for a point that means there is none: {:?}",
        h.runtime.backend.boundary().map(|b| b.mechanism)
    );
    // A box that cannot make a cgroup seats no shell (said on the hub), so the command half is
    // asked of the boxes that can: the point is what this test is about.
    if h.config().allow_bash {
        let out = call(&h, "bash", serde_json::json!({ "command": "echo hi" }));
        assert_eq!(
            out.outcome,
            letibot_transcript::ToolOutcome::Ok,
            "a command at the host point did not run: {}",
            out.payload
        );
        assert!(out.payload.contains("hi"), "{}", out.payload);
    }
}
