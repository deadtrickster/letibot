//! **The session list's drawing, pinned** — the picture the picker drew on 2026-10-27, before the
//! list became a built-in plugin, and the assertions the port must keep true.
//!
//! `docs/plugins-plan.md` §7 step 10 moves the decisions this fixture exercises — which groups
//! exist, what a row says, what `WAITING ON YOU` means, which rows fold — out of the head and into
//! `plugins/builtin/sessions`. A port that changed what a person sees while claiming to move
//! policy would be a port nobody could review, so the drawing is pinned FIRST: two committed
//! pictures of one daemon (as the picker opens, and with every fold opened), taken from the head's
//! own list before a line of the port existed.
//!
//! The plugin path is held to these same pictures by
//! `the_heads_list_and_the_plugins_list_draw_the_same_rows`, which is written for the port: two
//! paths, one daemon, one width, and the drawings must be identical.
//!
//! # Regenerating a picture
//!
//! ```sh
//! LETIBOT_BLESS=1 cargo test --offline -p letibot-tui sessions_list
//! ```
//!
//! is the only way to change `data/sessions-list-*.txt`, and it is meant to be used when a change
//! to the list is deliberate: the diff IS the change, and it belongs in the commit message.

use super::*;
use letibot_sessionlog::{NagPause, SessionOrigin};

/// A daemon with one of every kind of row, and sessions that say every word a row can say: a
/// slug, `WAITING ON YOU`, paused nags, another model, children at two depths.
fn the_daemon() -> App {
    let queue = letibot_sessionlog::gatekeeper_session_for_key("0123456789ab");
    let mut a = app();
    // **The head's home is the FIXTURE's, not the runner's.** Paths are drawn as `~/…` against
    // the home the head holds (`App::home`, which production reads from the environment once), so
    // a picture is reproducible on a box whose `HOME` is `/home/dead` and on one whose is not —
    // which is the difference that made these two tests red in the gate and green here.
    a.home = HOME.to_string();
    let mut habr = brief("s-habr", "the habr article", true);
    habr.slug = "habr".into();
    habr.stored_items = 412;
    habr.status.heads = 2;
    let mut rust = brief("s-rust", "the rust rewrite", false);
    rust.live = false;
    rust.waits_on_operator = 2;
    rust.wiring = letibot_sessionlog::SessionWiring {
        model: "glm-coding/glm-4.7".into(),
        ..wiring()
    };
    let mut paused = brief("s-paused", "a conversation with a quiet board", false);
    paused.live = false;
    paused.nags_paused = Some(NagPause::Until {
        ms: 1_900_000_000_000,
    });
    let mut sub = brief("s-sub-1", "read the repo", false);
    sub.parent_session_id = Some("s-habr".into());
    sub.slug = "read-it".into();
    let mut sub2 = brief("s-sub-2", "run the tests", false);
    sub2.parent_session_id = Some("s-habr".into());
    let mut deep = brief("s-sub-2-deep", "and again", false);
    deep.parent_session_id = Some("s-sub-2".into());
    let mut reviewer = brief(
        &format!("{queue}-sub-1791759365511364772"),
        "You are the gatekeeper. review it",
        false,
    );
    reviewer.parent_session_id = Some(queue.clone());
    let mut in_queue = brief("s-queue", "the review queue", false);
    in_queue.origin = SessionOrigin::Queue;
    let mut package = brief("s-pkg-1", "make me a landing page", false);
    package.origin = SessionOrigin::Package;
    let mut shot = brief("s-shot-1", "one prompt, one answer", false);
    shot.origin = SessionOrigin::OneShot;
    a.apply(hello(
        "s-habr",
        vec![
            habr, rust, paused, sub, sub2, deep, in_queue, reviewer, package, shot,
        ],
        Hub::new("s-habr").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    a
}

/// **The home this fixture's daemon is read under** — deliberately not whatever `$HOME` the test
/// process happens to have: the picture must be the same file on every runner.
const HOME: &str = "/home/dead";

/// The list as a person reads it, one line per drawn row, at `w` columns.
fn drawn(a: &App, w: usize) -> String {
    a.picker_lines(w).join("\n")
}

/// What `data/<name>` holds, or — under `LETIBOT_BLESS` — whatever is drawn now, written down.
fn golden(name: &str, now: String) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/app/tests/data")
        .join(name);
    if std::env::var("LETIBOT_BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the data dir");
        std::fs::write(&path, &now).expect("write the picture");
        return now;
    }
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — regenerate it with LETIBOT_BLESS=1",
            path.display()
        )
    })
}

/// **The picker as it opens** — the project's line, its conversations, and the `system` branch as
/// one closed line.
///
/// The operator, on this pane: *"make sessions tree collapsed by default"*. So the golden is the
/// folded picture, and it is the one a person sees on `ctrl-s` — a project holding the session they
/// are in (open, because a list that hides where you are cannot answer *where am I*), the rows of
/// that project, and every other branch as a line.
#[test]
fn the_session_list_looks_like_this_as_it_opens() {
    let a = the_daemon();
    let now = drawn(&a, 100);
    assert_eq!(golden("sessions-list-closed.txt", now.clone()), now);
    // The picture is not an empty one: a golden regenerated from a head that drew nothing would
    // pass for ever.
    assert!(now.contains("the habr article"), "{now}");
    assert!(now.contains("system ("), "{now}");
    // **And no group under it** — the branch is closed, so `system`'s own line is the whole of it.
    assert!(!now.contains("reviewers ("), "the folded picture: {now}");
    assert!(!now.contains("gatekeepers ("), "{now}");
}

/// **The same daemon with every fold opened** — the rows a reader sees after pressing → on each
/// line, and the nesting inside a group.
#[test]
fn the_session_list_looks_like_this_with_every_fold_open() {
    let mut a = the_daemon();
    // **The folds are the drawn list's**, keyed by row id — the head's own tree reads the same
    // state, and `set_row_fold` keeps both in step (`app::sessions_pane`). Opening a project puts
    // rows on the list that were not there to open, so the walk runs until it adds nothing.
    for _ in 0..4 {
        let keys: Vec<String> = a
            .session_rows()
            .iter()
            .map(|r| match r.idx {
                Some(i) => a.sessions[i].session_id.clone(),
                None => r.node.key().to_string(),
            })
            .collect();
        for key in keys {
            a.set_row_fold(&key, true);
        }
    }
    let now = drawn(&a, 100);
    assert_eq!(golden("sessions-list-open.txt", now.clone()), now);
    assert!(now.contains("read the repo"), "{now}");
    assert!(now.contains("review it"), "{now}");
    assert!(now.contains("reviewers (1)"), "{now}");
}

/// **The drawn picture does not depend on the runner's `HOME`.** The rows say `~/Projects/letibot`
/// because THIS head's home is the one the fixture set, whatever the process was started with —
/// the defect that made these tests green here and red in the gate, where `HOME` was `/home/dead`.
#[test]
fn the_workspace_is_shortened_against_the_heads_own_home() {
    let a = the_daemon();
    let text = drawn(&a, 100);
    assert!(
        text.contains("s-habr  ~/Projects/letibot"),
        "the fixture's home is the one paths are shortened against:\n{text}"
    );
    assert!(
        !text.contains("/home/dead/Projects/letibot"),
        "and never the absolute path when the head's own home prefixes it:\n{text}"
    );
    // A workspace OUTSIDE that home is drawn whole — the other half of the rule.
    let mut a = the_daemon();
    a.home = "/somewhere/else".into();
    assert!(
        drawn(&a, 100).contains("s-habr  /home/dead/Projects/letibot"),
        "a path under another home is not shortened"
    );
}

/// **The head's own list IS the plugin's list** — one row per row, the same order, the same words,
/// at the same width.
///
/// This is the assertion the port is judged by, and it was written before the port: the built-in
/// `sessions` layer decides the tree's shape and the words now, and the head's own
/// [`App::session_list`] is what a head draws when that layer cannot answer. Two paths, one
/// daemon, one width — the drawings must be identical, so a change to one side alone is a red test
/// rather than a difference nobody notices until a layer breaks.
#[test]
fn the_heads_list_and_the_plugins_list_draw_the_same_rows() {
    let a = the_daemon();
    let list = a
        .sessions_list(100)
        .expect("the built-in sessions layer answers in this process");
    // The rows the keys act on are the list's own — the same enumeration the `switch` acts
    // read, so a drawing and a keypress cannot come to different conclusions about row four.
    assert_eq!(
        list.rows.len(),
        list.content.stop_rows.len(),
        "one stop per row that can be acted on"
    );
    let (own, plugins) = both_lists(&a);
    assert_eq!(own, plugins, "the plugin's list is the head's list");
}

/// **A daemon whose head is standing in `here`** — the family view, and the state a head is in
/// after Enter on a sub-session.
fn standing_in(here: &str, sessions: Vec<SessionBrief>) -> App {
    let mut a = app();
    a.home = HOME.to_string();
    a.apply(hello(here, sessions, Hub::new(here).snapshot()));
    a.push_surface(Surface::Picker);
    a
}

/// The two drawings of one daemon at one width: the head's own list, and the layer's.
fn both_lists(a: &App) -> (Vec<String>, Vec<String>) {
    let plugins = crate::ui::render::row_strings(
        &a.sessions_list(100)
            .expect("the built-in sessions layer answers in this process")
            .content
            .lines,
        a.cfg.palette(),
    );
    let own = crate::ui::render::row_strings(&a.session_list(100).content.lines, a.cfg.palette());
    (own, plugins)
}

/// **A family is drawn whatever group its members are in** — a review's parent is the gatekeeper
/// (`system = "gatekeepers"`) and the review itself is a `reviewer`.
///
/// The layer walked the family in the PARENT's group, so its list was one row — the gatekeeper —
/// with the session the head was standing in missing from it. The head's family walk has no
/// branch at all, and the two lists are one list.
#[test]
fn the_family_of_a_review_is_drawn_by_both_lists() {
    let queue = letibot_sessionlog::gatekeeper_session_for_key("0123456789ab");
    let reviewer = format!("{queue}-sub-1791759365511364772");
    let mut gatekeeper = brief(&queue, "the review queue", false);
    gatekeeper.origin = SessionOrigin::Queue;
    let mut review = brief(&reviewer, "You are the gatekeeper. review it", false);
    review.parent_session_id = Some(queue.clone());
    let a = standing_in(&reviewer, vec![gatekeeper, review]);
    assert_eq!(
        a.session_rows().len(),
        2,
        "the head's family is the parent and the row it is standing in: {:?}",
        a.session_rows()
            .iter()
            .map(|r| r.idx.map(|i| a.sessions[i].title.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        a.session_rows()[0].open,
        Some(true),
        "the gatekeeper holds a row that is on the list, so it says `-`"
    );
    let (own, plugins) = both_lists(&a);
    assert_eq!(own, plugins, "the layer's family is the head's family");
    let gatekeeper_line = own
        .iter()
        .find(|l| l.contains("the review queue"))
        .unwrap_or_else(|| panic!("the gatekeeper's row is drawn: {own:?}"));
    assert!(
        gatekeeper_line.contains("- "),
        "and its glyph is open over the row below it: {gatekeeper_line:?}"
    );
    assert!(
        own.iter()
            .any(|l| l.contains("You are the gatekeeper. review it")),
        "the row the head is standing in is drawn: {own:?}"
    );
}

/// **The same rule on the other pair the tree creates**: a notes keeper spawned as a `task` child
/// of an ordinary conversation has a parent with no group at all, and it is that conversation's
/// child on both lists.
#[test]
fn the_family_of_a_keeper_is_drawn_by_both_lists() {
    let mut keeper = brief(
        "s-keeper",
        "You are the notes keeper. tidy the notes",
        false,
    );
    keeper.parent_session_id = Some("s-habr".into());
    let a = standing_in(
        "s-keeper",
        vec![brief("s-habr", "the habr article", false), keeper],
    );
    let (own, plugins) = both_lists(&a);
    assert_eq!(own, plugins, "the layer's family is the head's family");
    assert!(
        own.iter().any(|l| l.contains("the notes keeper")),
        "the keeper is drawn, and it is the row the head is standing in: {own:?}"
    );
}

/// **A title and a workspace are somebody's text, and both lists draw it the same way** — the
/// layer turned a control character into a space and the head did not, so one row was two
/// pictures, and one of them carried an escape to the terminal.
#[test]
fn a_control_character_in_a_row_is_drawn_as_a_space_by_both_lists() {
    let mut nasty = brief("s-nasty", "the\u{7}bell\u{1b}[31mred", false);
    nasty.wiring.workspace = "/home/dead/Projects/letibot/letibot\u{7}".into();
    let a = standing_in("s-nasty", vec![nasty]);
    let (own, plugins) = both_lists(&a);
    assert_eq!(own, plugins, "one row, one picture");
    assert!(
        !own.iter().any(|l| l.chars().any(char::is_control)),
        "and no control character reaches the screen:\n{}",
        own.join("\n")
    );
}

// ---------------------------------------------------------------------------
// **A session a session started has that session as its parent**
//
// The tree nests by `SessionBrief::parent_session_id` and by nothing else — not by the id, not by
// the workspace. The daemon side of the rule is `Registry::parent_for`, applied once in
// `Harness::open_with_registry`; these tests are the pen, and each one is a spawner whose child
// must land under its parent (or, for the two a PERSON starts, at the top of its project).
// ---------------------------------------------------------------------------

/// The rows `session_rows()` would draw, as `(session, depth)` — a node's line has no session, so
/// it is not here; see the file's note on the two indices.
fn tree(a: &App) -> Vec<(String, usize)> {
    a.session_rows()
        .iter()
        .filter_map(|r| r.idx.map(|i| (a.sessions[i].session_id.clone(), r.depth)))
        .collect()
}

/// Every fold open, so every child is on the list — the walk
/// `the_session_list_looks_like_this_with_every_fold_open` makes, as a helper for the tests that
/// care about nesting rather than about what a folded list looks like.
fn open_everything(a: &mut App) {
    for _ in 0..4 {
        let keys: Vec<String> = a
            .session_rows()
            .iter()
            .map(|r| match r.idx {
                Some(i) => a.sessions[i].session_id.clone(),
                None => r.node.key().to_string(),
            })
            .collect();
        for key in keys {
            a.set_row_fold(&key, true);
        }
    }
}

/// **A `task` child, and its own child under it** — the shape `HarnessTaskRunner::start` leaves:
/// an id minted `…-sub-<ns>`, and the child's own id as the grandchild's parent.
#[test]
fn a_task_child_and_its_own_child_are_drawn_under_their_parents() {
    let mut a = app();
    a.home = HOME.to_string();
    let root = brief("s-root", "the conversation", true);
    let mut child = brief("s-root-sub-1793000000000000000", "count the files", false);
    child.parent_session_id = Some("s-root".into());
    child.slug = "count-files".into();
    let mut grandchild = brief(
        "s-root-sub-1793000000000000000-sub-1793000000000000001",
        "and the sizes",
        false,
    );
    grandchild.parent_session_id = Some("s-root-sub-1793000000000000000".into());
    a.apply(hello(
        "s-root",
        vec![root, child, grandchild],
        Hub::new("s-root").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    open_everything(&mut a);
    assert_eq!(
        tree(&a),
        vec![
            ("s-root".to_string(), 1),
            ("s-root-sub-1793000000000000000".to_string(), 2),
            (
                "s-root-sub-1793000000000000000-sub-1793000000000000001".to_string(),
                3
            ),
        ],
        "a child is one level under its parent, and a grandchild one level under the child"
    );
    let text = drawn(&a, 100);
    assert!(text.contains("count the files"), "{text}");
    assert!(text.contains("and the sizes"), "{text}");
}

/// **A `task_start` child is placed by the tree it hangs from, not by its own worktree.**
///
/// The child's workspace is `…/worktrees/agent-a-thing` and its parent's is the repository, so a
/// head that placed rows by `wiring.workspace` would invent a second project line and put the
/// child at the top of it — which is the picture the operator reported as *subagents shown as
/// top level*.
#[test]
fn a_task_start_child_is_placed_by_its_parent_and_not_by_its_worktree() {
    let mut a = app();
    a.home = HOME.to_string();
    let root = brief("s-root", "the conversation", true);
    let mut child = brief(
        "s-root-sub-1793000000000000002",
        "the worktree brief",
        false,
    );
    child.parent_session_id = Some("s-root".into());
    child.wiring = letibot_sessionlog::SessionWiring {
        workspace: "/home/dead/Projects/letibot/worktrees/agent-a-thing".into(),
        ..wiring()
    };
    a.apply(hello(
        "s-root",
        vec![root, child],
        Hub::new("s-root").snapshot(),
    ));
    let place = a.placements();
    let at = |id: &str| {
        a.sessions
            .iter()
            .position(|s| s.session_id == id)
            .expect("on the feed")
    };
    assert_eq!(
        place[at("s-root-sub-1793000000000000002")].project,
        place[at("s-root")].project,
        "a worktree is where the child works, not where it is drawn"
    );
    assert_eq!(
        a.projects(&place).len(),
        1,
        "one repository is one project line, however many worktrees its children hold"
    );
    a.push_surface(Surface::Picker);
    open_everything(&mut a);
    assert_eq!(
        tree(&a),
        vec![
            ("s-root".to_string(), 1),
            ("s-root-sub-1793000000000000002".to_string(), 2),
        ]
    );
}

/// **A session a PERSON started is a top row, and nothing invents a parent for it** — the
/// corollary of the rule, tested because a rule that fills a parent in is one edit away from
/// inventing one.
///
/// The two rows are the two doors: `--session SESSION` (a conversation reopened by id) and
/// `--continue`'s newest conversation. The second one's id is deliberately shaped like a minted
/// child's (`…-sub-3`), because the tree must read the field and not the name: `--continue`
/// resolves only rows whose parent is NULL, so this is a session a person is in.
#[test]
fn a_session_a_person_started_is_a_top_row_whatever_its_id_looks_like() {
    let mut a = app();
    a.home = HOME.to_string();
    let reopened = brief(
        "s-1793000000000000003",
        "the session --session reopened",
        true,
    );
    let continued = brief(
        "s-1793000000000000004-sub-3",
        "the newest conversation",
        false,
    );
    a.apply(hello(
        "s-1793000000000000003",
        vec![reopened, continued],
        Hub::new("s-1793000000000000003").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    open_everything(&mut a);
    assert_eq!(
        tree(&a),
        vec![
            ("s-1793000000000000003".to_string(), 1),
            ("s-1793000000000000004-sub-3".to_string(), 1),
        ],
        "neither has a parent, so neither is nested under the other"
    );
}

/// **A child whose parent has since closed is drawn under the project, not swallowed.**
///
/// The parent is not on the feed at all — a session the daemon no longer holds and the store no
/// longer lists — so `parent_on_list` answers `None` and the row is a root of its project. The
/// alternative (hiding it, because its parent is not there to be nested under) is the one this
/// pins against: a finished child's transcript is still a conversation a head can switch into.
#[test]
fn a_child_whose_parent_has_closed_is_drawn_under_the_project() {
    let mut a = app();
    a.home = HOME.to_string();
    let live = brief("s-live", "a conversation", true);
    let mut orphan = brief(
        "s-gone-sub-1793000000000000005",
        "a child of a closed session",
        false,
    );
    orphan.parent_session_id = Some("s-gone".into());
    a.apply(hello(
        "s-live",
        vec![live, orphan],
        Hub::new("s-live").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    open_everything(&mut a);
    assert_eq!(
        tree(&a),
        vec![
            ("s-live".to_string(), 1),
            ("s-gone-sub-1793000000000000005".to_string(), 1),
        ],
        "an orphan is not a subagent: it is a root of the project it belongs to"
    );
    assert!(
        drawn(&a, 100).contains("a child of a closed session"),
        "and it is on the screen rather than vanishing"
    );
}
