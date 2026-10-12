//! **R69: one surface stack** — the surface drawn is the surface the keys reach, a covered one
//! keeps its own scroll, and what owns Enter is a property of the surface in front.

use super::*;

/// A head with one subagent row, so the tree's Enter has somewhere to switch to.
fn with_a_subagent() -> App {
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "summarize ~/bin/letibot".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
    a
}

/// **Two panes open, and the keys reach the one on the screen** — the review's worst head
/// finding, from the other side. `ctrl-g` then `ctrl-t`: the todos pane was drawn (draw order
/// todos > subagents) while ↑↓ and Enter went to the subagents list (key order subagents >
/// todos), and **Enter switched session into a row nobody saw**.
#[test]
fn with_two_panes_open_the_keys_reach_the_one_drawn() {
    let mut a = with_a_subagent();
    a.key(Key::CtrlG);
    a.key(Key::CtrlT);
    let shown = a.screen(100, 30).join("\n");
    assert!(
        !shown.contains("summarize ~/bin/letibot"),
        "the subagents list is covered:\n{shown}"
    );
    let r = a.key(Key::Enter);
    assert_ne!(
        r,
        Some(Action::Switch("s-sub-1".into())),
        "Enter switched into a subagent row nobody could see"
    );
}

/// **Esc pops one surface, and the one under it comes back as it was** — its cursor and its
/// scroll kept in its own entry, where the shared `pane_scroll` used to be moved by whichever
/// pane the keys happened to reach.
#[test]
fn esc_pops_the_top_and_the_covered_pane_keeps_its_scroll() {
    let mut a = with_a_subagent();
    a.key(Key::CtrlG);
    a.pane_scroll = 7;
    a.key(Key::CtrlT);
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert_eq!(a.pane_scroll, 0, "the new surface opens at its head");
    a.pane_scroll = 2;
    a.key(Key::Esc);
    assert_eq!(
        a.top_surface(),
        Some(Surface::Subagents),
        "one Esc, one surface"
    );
    assert_eq!(a.pane_scroll, 7, "the tree came back where it was left");
    let shown = a.screen(100, 30).join("\n");
    assert!(shown.contains("summarize ~/bin/letibot"), "{shown}");
    a.key(Key::Esc);
    assert_eq!(a.top_surface(), None);
}

/// **A chord for a covered pane brings it forward**; only the pane in front is closed by its
/// own chord. `ctrl-t`, `ctrl-g`, `ctrl-t` shows the todos pane — it does not close a pane the
/// operator cannot see and leave the tree they were not looking for.
#[test]
fn a_chord_brings_a_covered_pane_forward_and_closes_the_one_in_front() {
    let mut a = with_a_subagent();
    a.key(Key::CtrlT);
    a.key(Key::CtrlG);
    a.key(Key::CtrlT);
    assert_eq!(a.top_surface(), Some(Surface::Todos));
    assert!(
        a.is_open(Surface::Subagents),
        "the tree is still open behind it"
    );
    a.key(Key::CtrlT);
    assert_eq!(a.top_surface(), Some(Surface::Subagents));
}

/// **A pane under rano takes no keys** — `ctrl-]` back to the composer over an open jobs pane
/// left the hint bar saying "enter sends" while Enter was swallowed by the hidden pane (the
/// review's finding 4, second half).
#[test]
fn a_pane_under_the_editor_does_not_swallow_the_composers_enter() {
    let mut a = app();
    a.key(Key::CtrlQ);
    a.open_editor();
    // `ctrl-]` from inside rano: the keyboard is the composer's, rano stays drawn.
    a.edit_pane.as_mut().expect("just opened").focused = false;
    assert!(
        a.hint_bar(200).contains("enter sends"),
        "{}",
        a.hint_bar(200)
    );
    typed(&mut a, "hi");
    assert_eq!(a.key(Key::Enter), Some(Action::Prompt("hi".into())));
}

/// **The merge queue owns Enter like every other pane** — it was missing from the list that
/// said so, and Enter with a typed line sent the prompt over an empty queue (and opened an
/// entry over a non-empty one).
#[test]
fn the_queue_pane_owns_enter_even_when_it_is_empty() {
    let mut a = app();
    a.command("queue");
    typed(&mut a, "hi");
    assert_eq!(
        a.key(Key::Enter),
        None,
        "the line was sent from under the queue"
    );
    assert_eq!(a.editor.text(), "hi", "and it is still the operator's");
}

/// **A setting card is in front of a pane**: its arrows and its Esc are the card's, and the
/// pane under it is still there when the card goes.
#[test]
fn a_setting_card_over_a_pane_takes_esc_first() {
    let mut a = with_a_subagent();
    a.key(Key::CtrlG);
    a.command("verbosity");
    assert!(a.pick.is_some());
    a.key(Key::Esc);
    assert!(a.pick.is_none(), "the card went first");
    assert_eq!(
        a.top_surface(),
        Some(Surface::Subagents),
        "and only the card"
    );
}

/// **rano under the diff popup is not drawn, so it has neither the keys nor the mouse** —
/// `editor_drawn` asked about a `!term` pane only, so `route` would have handed rano the keys of
/// a screen showing the popup.
#[test]
fn the_editor_under_the_diff_popup_takes_no_keys() {
    let mut a = app();
    a.open_editor();
    assert!(a.editor_focused(), "the premise: rano has the keyboard");
    a.open_diff_popup(&FileRef::file("notes.txt"));
    assert!(!a.editor_drawn(), "the popup is in front of rano");
    let (keys, rano_took) = a.route(vec![rano::term::Event::Key(rano::term::KeyEvent::plain(
        rano::term::KeyCode::Char('x'),
    ))]);
    assert!(!rano_took, "rano was handed a key under the popup");
    assert_eq!(keys, vec![Key::Char('x')]);
}
