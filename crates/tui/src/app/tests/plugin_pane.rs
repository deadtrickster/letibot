//! **A plugin's pane**: the document a `pane_<name>` hook answered, held by name and drawn
//! under the host's keys (`docs/plugins-slice1.md` C3/C4).
//!
//! Every test here starts from the JSON a plugin would answer — the wire's own shape — because
//! that is what the head parses: a fixture written as a `View` would prove the types agree with
//! themselves and nothing about what a plugin has to send.

use super::*;

/// **The name the daemon publishes a pane under**: `<layer name>/<hook name>`.
const PANE: &str = "plan-dashboard/dashboard";

/// **A dashboard's view**, as the plugin wrote it: two panels that draw and one that could not,
/// a finding with everything a reader checks it by, a legend, a badge, and one block kind this
/// head does not know.
fn dashboard() -> serde_json::Value {
    serde_json::json!({
        "title": "stroppy-conformity · v0.2.0",
        "badge": {"text": "3 running", "role": "attention"},
        "panels": [
            {"id": "tiers", "key": "t", "title": "tiers", "denominator": "rows in the tier",
             "summary": [{"lines": [
                {"label": "tier 1", "value": "14/22", "bar": {"frac": 0.64},
                 "tail": "3 in progress", "id": "tier1", "detail": "the tier's own words",
                 "actions": [{"key": "s", "label": "start", "say": "start tier 1"}]},
                {"label": "tier 2", "value": "3/9", "bar": {"frac": 0.33}, "id": "tier2",
                 "detail": "tier two's words",
                 "actions": [{"key": "e", "label": "expand", "event": "expand"}]}
             ]}],
             "view": [{"table": {"columns": ["item", "status"],
                "rows": [{"id": "tier1/jstests", "cells": ["Mongo jstests", "not started"],
                          "detail": "the jstests work"}]}}]},
            {"id": "hosts", "key": "h", "title": "hosts",
             "summary": [
                {"kv": {"rows": [["lab2x1", "busy"], ["lab2x2", "idle"]]}},
                {"progress": {"done": 41, "of": 96}}
             ]},
            {"id": "vault", "key": "v", "title": "vault",
             "unavailable": {"why": "cannot connect to lab2x1", "fix": "add the lab2x1 key to the agent"}}
        ],
        "findings": [{"id": "stale-status", "what": "status file is 3 days old", "panel": "tiers",
                      "severity": "tripped", "detail": "nobody has written the file since Tuesday",
                      "evidence": {"updated": "2026-10-22T21:55:00Z", "age_h": 74},
                      "fix": "write the status file",
                      "verify": "cat .letibot/status.json",
                      "actions": [{"key": "r", "label": "refresh", "look": true}]}],
        "legend": {"tier": "a row of the plan", "host": "a machine the fleet runs on",
                   "tier 1": "the tier the release waits on"},
        "every_s": 30
    })
}

/// A head holding the dashboard, with the pane not yet opened.
fn holding(view: serde_json::Value) -> App {
    let mut a = app();
    a.session_id = "s1".into();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::PluginPane {
            pane: PANE.into(),
            view,
        },
    )));
    a
}

/// A head with the pane open, which is the state every act below starts from.
fn opened() -> App {
    let mut a = holding(dashboard());
    let asked = a.command(&format!("pane {PANE}"));
    assert_eq!(
        asked,
        Some(Action::PaneOpen {
            pane: PANE.into(),
            // **The open can only guess the rectangle** — this head has never drawn a pane, so
            // it says `0` and the frame that draws it sends the real one.
            w: 0,
            h: 0,
        }),
        "opening the pane asks the daemon for a fresh look"
    );
    a
}

/// The screen row a needle was drawn on, and its column — for the tests that point at a row.
fn find(screen: &[String], needle: &str) -> (u16, u16) {
    for (y, row) in screen.iter().enumerate() {
        if let Some(at) = row.find(needle) {
            return (visible_width(&row[..at]) as u16, y as u16);
        }
    }
    panic!("`{needle}` is not on the screen:\n{}", screen.join("\n"));
}

/// The screen as one string, for the assertions that only ask *is this on it*.
fn shown(a: &mut App, w: usize, h: usize) -> String {
    a.screen(w, h).join("\n")
}

/// **The overview is the whole language at once**: the title, the findings summary line, each
/// panel's title with its denominator, and the one grid — every panel's numbers in the same
/// columns, which is the thing a per-panel layout cannot do.
#[test]
fn the_overview_draws_the_title_the_findings_and_one_grid_across_panels() {
    let mut a = opened();
    let screen = shown(&mut a, 110, 40);
    assert!(screen.contains("stroppy-conformity · v0.2.0"), "{screen}");
    assert!(screen.contains("1 tripped"), "the summary line: {screen}");
    assert!(
        screen.contains("tiers") && screen.contains("rows in the tier"),
        "a panel's title carries its denominator: {screen}"
    );
    assert!(screen.contains("hosts"), "{screen}");
    // **The one grid**: the two panels' values end in the same column, so a reader can compare
    // them down the frame — the whole reason there is one layout function.
    let lines = a.screen(110, 40);
    let end = |needle: &str| {
        lines.iter().find(|l| l.contains(needle)).map(|l| {
            let at = l.find(needle).unwrap_or(0);
            // Columns, not bytes: a `▸` is three bytes and one column.
            visible_width(&l[..at]) + visible_width(needle)
        })
    };
    assert_eq!(
        end("14/22"),
        end("3/9"),
        "the two panels' values are in one column: {screen}"
    );
    assert!(screen.contains('█'), "a bar is drawn: {screen}");
    assert!(screen.contains("3 in progress"), "and its tail: {screen}");
}

/// **A panel that could not get its data says why and how to fix it** — and draws none of its
/// blocks: an empty result and a broken look are different facts and must not look the same.
#[test]
fn an_unavailable_panel_shows_the_reason_and_the_fix() {
    let mut a = opened();
    let screen = shown(&mut a, 110, 40);
    assert!(screen.contains("vault"), "{screen}");
    assert!(screen.contains("cannot connect to lab2x1"), "{screen}");
    assert!(
        screen.contains("fix add the lab2x1 key to the agent"),
        "{screen}"
    );
}

/// **A panel's key opens its view, and Esc gives the overview back** — C3's ladder, one step.
#[test]
fn a_panels_key_opens_its_view_and_esc_returns() {
    let mut a = opened();
    assert!(a.screen(110, 40).join("\n").contains("tier 1"));
    assert_eq!(a.key(Key::Char('t')), None);
    let inside = a.screen(110, 40).join("\n");
    assert!(
        inside.contains("Mongo jstests"),
        "the panel's own view, not its summary: {inside}"
    );
    assert!(
        !inside.contains("tier 2"),
        "the summary is not the view: {inside}"
    );
    assert_eq!(
        a.top_surface(),
        Some(Surface::PluginPane),
        "a view is not a surface of its own"
    );
    a.key(Key::Esc);
    let back = a.screen(110, 40).join("\n");
    assert!(back.contains("tier 2"), "the overview came back: {back}");
    assert_eq!(
        a.top_surface(),
        Some(Surface::PluginPane),
        "esc left the view, not the pane"
    );
    a.key(Key::Esc);
    assert_eq!(a.top_surface(), None, "the second esc closes the pane");
}

/// **`f` is the findings list, and Enter reads one whole**: its detail, the numbers behind it,
/// what to do, and how to check it yourself — the whole of a finding, which is the point of
/// collecting them.
#[test]
fn f_lists_the_findings_and_enter_reads_one_whole() {
    let mut a = opened();
    a.key(Key::Char('f'));
    let list = a.screen(110, 40).join("\n");
    assert!(list.contains("status file is 3 days old"), "{list}");
    assert!(
        list.contains("tripped"),
        "the severity is on the row: {list}"
    );
    a.key(Key::Enter);
    assert_eq!(a.top_surface(), Some(Surface::PluginDetail));
    let detail = a.screen(110, 40).join("\n");
    assert!(detail.contains("nobody has written the file"), "{detail}");
    assert!(detail.contains("evidence"), "{detail}");
    assert!(detail.contains("age_h"), "{detail}");
    assert!(detail.contains("74"), "the number behind it: {detail}");
    assert!(detail.contains("write the status file"), "{detail}");
    assert!(detail.contains("cat .letibot/status.json"), "{detail}");
    a.key(Key::Esc);
    assert_eq!(
        a.top_surface(),
        Some(Surface::PluginPane),
        "esc comes back to the pane"
    );
    assert!(
        a.screen(110, 40).join("\n").contains("tripped"),
        "on its list"
    );
}

/// **`l` is the legend**: every label the pane uses, and what it means.
#[test]
fn l_shows_the_legend() {
    let mut a = opened();
    a.key(Key::Char('l'));
    let screen = a.screen(110, 40).join("\n");
    assert!(screen.contains("tier"), "{screen}");
    assert!(screen.contains("a row of the plan"), "{screen}");
    assert!(screen.contains("a machine the fleet runs on"), "{screen}");
}

/// **↓ moves the selection through the stops**, and the mark moves with it — the one
/// enumeration the arrows, Enter, the action keys and a click all read.
#[test]
fn down_moves_the_selection_through_the_stops() {
    let mut a = opened();
    a.screen(110, 40);
    assert_eq!(a.plugin_sel, 0);
    let first = a.screen(110, 40).join("\n");
    assert!(
        first.lines().any(|l| l.contains("▸ tier 1")),
        "the cursor starts on the first stop: {first}"
    );
    a.key(Key::Down);
    assert_eq!(a.plugin_sel, 1);
    let second = a.screen(110, 40).join("\n");
    assert!(
        second.lines().any(|l| l.contains("▸ tier 2")),
        "the mark moved: {second}"
    );
    assert!(
        !second.lines().any(|l| l.contains("▸ tier 1")),
        "and left the one it was on: {second}"
    );
}

/// **An action key sends the stop's `event` back to the pane that drew it** — the `item` is the
/// row's id, and the action is the word the plugin wrote.
#[test]
fn an_action_key_queues_a_pane_action() {
    let mut a = opened();
    a.screen(110, 40);
    a.key(Key::Down);
    assert_eq!(
        a.key(Key::Char('e')),
        Some(Action::PaneAction {
            pane: PANE.into(),
            item: "tier2".into(),
            action: "expand".into(),
        })
    );
}

/// **A `say` action submits the line through the composer's own path**, so it is queued and
/// announced exactly as a typed prompt is — and attributed to the operator, who pressed the
/// key. It is not a `PaneAction`: a pane does not get a second door into the model.
#[test]
fn a_say_action_submits_the_line_as_a_prompt() {
    let mut a = opened();
    a.screen(110, 40);
    assert_eq!(
        a.key(Key::Char('s')),
        Some(Action::Prompt("start tier 1".into()))
    );
}

/// **A `look` action re-runs the pane's look** — C3's third kind, and the one that sends the
/// same `look` word the wire spells.
#[test]
fn a_look_action_asks_for_a_fresh_look() {
    let mut a = opened();
    a.key(Key::Char('f'));
    assert_eq!(
        a.key(Key::Char('r')),
        Some(Action::PaneAction {
            pane: PANE.into(),
            item: "stale-status".into(),
            action: "look".into(),
        })
    );
}

/// **A block kind this host does not draw is one muted line naming it** — never a silently
/// missing row: the plugin's answer is not wrong and this head is old.
#[test]
fn an_unknown_block_kind_is_a_muted_line_naming_it() {
    let mut a = opened();
    let screen = shown(&mut a, 110, 40);
    assert!(
        screen.contains("progress — this head does not draw that block kind"),
        "{screen}"
    );
}

/// **The badge is drawn in the status row**, like the waits-on-you counter — the pane's own
/// counter, in the register the pane chose, and only while that pane is on the screen.
#[test]
fn the_badge_is_drawn_in_the_status_row() {
    let mut a = opened();
    let screen = shown(&mut a, 110, 40);
    assert!(screen.contains("3 running"), "the badge: {screen}");
    // And with the pane closed it is gone: a counter about a screen nobody is looking at is
    // furniture.
    a.key(Key::Esc);
    let closed = shown(&mut a, 110, 40);
    assert!(!closed.contains("3 running"), "{closed}");
}

/// **A click on a panel's title opens its view** — C3's one click, recorded by the frame that
/// drew the title.
#[test]
fn a_click_on_a_panel_opens_its_view() {
    let mut a = opened();
    a.screen(110, 40);
    // The frame's own record: which row each panel's title was drawn on, in terminal cells.
    let hit = a
        .hits
        .iter()
        .find(|h| h.target == Target::PluginPanel)
        .cloned()
        .expect("the frame recorded the first panel's title");
    a.key(Key::Click {
        x: 12,
        y: hit.rect.y as u16,
    });
    let screen = a.screen(110, 40).join("\n");
    assert!(
        screen.contains("Mongo jstests"),
        "the panel's view opened: {screen}"
    );
}

/// **A pane the head cannot honour is refused rather than drawn**, and says why: a view whose
/// action key is `q` would let a plugin own the key the operator leaves with. The verdict is
/// the same one the daemon runs (`View::validate`), and it is drawn where the dashboard would
/// have been.
#[test]
fn a_view_that_collides_with_the_hosts_keys_is_refused_and_says_why() {
    let view = serde_json::json!({
        "title": "a bad pane",
        "panels": [{"id": "p", "key": "p", "title": "p", "summary": [{"lines": [
            {"label": "a", "id": "a", "actions": [{"key": "q", "label": "quit", "say": "go"}]}
        ]}]}]
    });
    let mut a = holding(view);
    a.command(&format!("pane {PANE}"));
    let screen = a.screen(110, 30).join("\n");
    assert!(
        screen.contains("host"),
        "the refusal names the rule: {screen}"
    );
    assert!(
        !screen.contains("a bad pane"),
        "and the pane is not drawn as though it were fine: {screen}"
    );
}

/// **`/pane` lists what the session has, by title**, and `/pane NAME` opens one — the panes
/// list, on a head whose panes are the daemon's and cannot be a fixed table.
#[test]
fn the_pane_verb_lists_them_by_title_and_opens_by_name() {
    let mut a = holding(dashboard());
    a.command("pane");
    let list = a.screen(110, 30).join("\n");
    assert!(
        list.contains("stroppy-conformity · v0.2.0"),
        "the list is by title: {list}"
    );
    assert!(list.contains(&format!("/pane {PANE}")), "{list}");
    a.key(Key::Esc);
    a.command(&format!("pane {PANE}"));
    assert!(a.is_open(Surface::PluginPane));
    // A name the session does not have is said, not opened.
    let mut b = holding(dashboard());
    b.command("pane nobody/nothing");
    assert!(!b.is_open(Surface::PluginPane));
    assert!(
        b.screen(110, 30).join("\n").contains("no plugin pane"),
        "the refusal names it"
    );
}

/// **Esc at the overview closes the pane AND tells the daemon** — C4's `PaneClose`, and the only
/// thing that takes a pane off the host's refresh clock. The view below it is a step of Esc's
/// ladder and says nothing: leaving a panel's view is not leaving the pane.
#[test]
fn esc_at_the_overview_closes_the_pane_and_tells_the_daemon() {
    let mut a = opened();
    a.screen(110, 40);
    // The frame's own correction of the rectangle the open could only guess, and whatever else a
    // draw queued: drained so the assertions below are about the keys.
    a.take_actions();

    a.key(Key::Char('t'));
    assert_eq!(a.key(Key::Esc), None, "esc left the view, not the pane");
    assert_eq!(a.top_surface(), Some(Surface::PluginPane));
    assert!(
        a.take_actions().is_empty(),
        "leaving a view is not a pane close"
    );

    assert_eq!(
        a.key(Key::Esc),
        Some(Action::PaneClose { pane: PANE.into() }),
        "the pane left the screen, so the daemon stops refreshing it"
    );
    assert!(!a.is_open(Surface::PluginPane));
    assert_eq!(a.plugin_open, None, "and the name went with it");
    // **And the view this head holds stays** — it is the daemon's answer about the session, not
    // about this screen, so reopening the pane draws it while the new look travels.
    assert!(a.plugin_panes.contains_key(PANE));
}

/// **A label the view's legend documents answers on hover** — C3's second use of the legend
/// (`the same text answers on hover`): point at a row and the bar says what its label means.
#[test]
fn hovering_a_documented_label_shows_the_legends_words() {
    let mut a = opened();
    let screen = a.screen(110, 40);
    let (x, y) = find(&screen, "tier 1");
    a.key(Key::Hover { x: x + 1, y });
    let hovered = a.screen(110, 40).join("\n");
    assert!(
        hovered.contains("the tier the release waits on"),
        "the legend's text for the row's label: {hovered}"
    );

    // **A label the legend does not document says nothing extra** — `tier 2` is not a key, and
    // the bar keeps its key hints rather than guessing at a meaning.
    let (x, y) = find(&screen, "tier 2");
    a.key(Key::Hover { x: x + 1, y });
    let plain = a.screen(110, 40).join("\n");
    assert!(!plain.contains("the tier the release waits on"), "{plain}");
}

/// **A pane the daemon says is gone closes**, and its view goes with it: a surface left holding
/// a name nobody answers is a screen whose keys do nothing.
#[test]
fn a_gone_pane_closes_and_is_dropped() {
    let mut a = opened();
    assert!(a.is_open(Surface::PluginPane));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::PluginPaneGone { pane: PANE.into() },
    )));
    assert!(!a.is_open(Surface::PluginPane), "the pane closed");
    assert!(a.plugin_panes.is_empty(), "and the view went with it");
}

/// **The arrow keys do not reach the transcript behind the pane** — the wheel router's rule,
/// and the scroll guard: a pane on top takes them.
#[test]
fn the_wheel_over_the_pane_scrolls_the_pane() {
    let mut a = opened();
    a.screen(110, 12);
    let before = a.pane_scroll;
    a.key(Key::Wheel {
        up: false,
        x: 10,
        y: 6,
    });
    assert_eq!(a.scroll, 0, "the conversation did not move");
    assert!(
        a.pane_scroll > before,
        "the pane scrolled instead: {} → {}",
        before,
        a.pane_scroll
    );
}
