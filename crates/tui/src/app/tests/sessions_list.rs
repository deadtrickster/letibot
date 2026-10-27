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

/// **The picker as it opens** — `mine` above every fold, the four automatic kinds closed, and the
/// facts on each row.
#[test]
fn the_session_list_looks_like_this_as_it_opens() {
    let a = the_daemon();
    let now = drawn(&a, 100);
    assert_eq!(golden("sessions-list-closed.txt", now.clone()), now);
    // The picture is not an empty one: a golden regenerated from a head that drew nothing would
    // pass for ever.
    assert!(now.contains("the habr article"), "{now}");
    assert!(now.contains("subagents (3)"), "{now}");
}

/// **The same daemon with every fold opened** — the rows a reader sees after pressing → on each
/// group's line, and the nesting inside a group.
#[test]
fn the_session_list_looks_like_this_with_every_fold_open() {
    let mut a = the_daemon();
    // **The folds are the drawn list's**, keyed by row id — the head's own `open_groups` is the
    // way out's state, and `set_row_fold` keeps both in step (`app::sessions_pane`).
    for group in letibot_sessionlog::SessionGroup::ALL {
        a.set_row_fold(group.title(), true);
    }
    a.set_row_fold("s-habr", true);
    let now = drawn(&a, 100);
    assert_eq!(golden("sessions-list-open.txt", now.clone()), now);
    assert!(now.contains("read the repo"), "{now}");
}

/// **The head's own list IS the plugin's list** — one row per row, the same order, the same words,
/// at the same width.
///
/// This is the assertion the port is judged by, and it was written before the port: the built-in
/// `sessions` layer decides the groups and the words now, and the head's `SessionPicker` is what a
/// head draws when that layer cannot answer. Two paths, one daemon, one width — the drawings must
/// be identical, so a change to one side alone is a red test rather than a difference nobody
/// notices until a layer breaks.
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
    let plugins = crate::ui::render::row_strings(&list.content.lines, a.cfg.palette());
    let own =
        crate::ui::render::row_strings(&a.session_picker().content(100).lines, a.cfg.palette());
    assert_eq!(own, plugins, "the plugin's list is the head's list");
}
