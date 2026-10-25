//! **R69: each surface's keys are declared once**, and the hint bar and `/help` are that table
//! read as words — so a binding appears in both, a plain letter never acts on a typed line, and
//! a chord the bar names is the chord the dispatch runs.

use super::*;
use crate::app::keys::bindings::{Guard, On, Says};

/// **A plain letter always carries the empty-composer guard** — the letters are the operator's
/// to type first. The queue pane's `a`/`v`/`d`/`r` had no guard where the subagents pane's
/// `o`/`p` did (R67 item 1); the table makes the rule checkable for every surface at once.
#[test]
fn every_plain_letter_binding_waits_for_an_empty_composer() {
    for s in Surface::ALL {
        for b in s.bindings() {
            if b.on.iter().any(|o| matches!(o, On::Char(_))) {
                assert_eq!(
                    b.guard,
                    Guard::EmptyComposer,
                    "{s:?} binds a letter that acts on a typed line: {b:?}"
                );
            }
        }
    }
}

/// **And a word typed with any surface open reaches the composer** — the behaviour the guard is
/// for, driven through the keys, for every surface a person can type over.
#[test]
fn a_word_typed_over_any_surface_reaches_the_composer() {
    for s in Surface::ALL {
        // The two reading surfaces take every key while they are up, on purpose: a surface with
        // no field on it must not grow a draft under it (`TODO_ROW`'s and `PLUGIN_DETAIL`'s last
        // binding).
        if matches!(s, Surface::TodoRow | Surface::PluginDetail) {
            continue;
        }
        let mut a = app();
        a.session_id = "s1".into();
        running_child(&mut a);
        a.apply(queue_frame(queues(
            vec![queue_review_row("c1", "waiting")],
            Vec::new(),
        )));
        a.push_surface(s);
        let mut sent = Vec::new();
        // From its second letter on, a word is a typed line, and no letter in it is a verb. (Its
        // FIRST letter on an empty composer is a key — that is what a pane letter is.)
        for c in "hello: deploy or approve".chars() {
            sent.extend(a.key(Key::Char(c)));
        }
        assert!(sent.is_empty(), "{s:?}: typing sent {sent:?}");
        assert_eq!(
            a.editor.text(),
            "hello: deploy or approve",
            "{s:?} ate letters of a typed word"
        );
    }
}

fn running_child(a: &mut App) {
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Subagent {
            subagent_id: "s-sub-1".into(),
            state: "running".into(),
            prompt: "a child".into(),
            role: "coder".into(),
            task: String::new(),
            model: String::new(),
            answer: None,
            slug: String::new(),
        },
    )));
}

/// **The bar and the help are the table, read as words**: every binding that says something is
/// on the bar while its surface is in front, and in the help row for its surface. Generated, so
/// this is the check that the generation reads every binding — a binding added later with words
/// cannot be missing from either.
#[test]
fn every_binding_that_says_something_is_on_the_bar_and_in_the_help() {
    let help = help_lines(&app().cfg, 400).join("\n");
    for s in Surface::ALL {
        let mut a = app();
        a.session_id = "s1".into();
        running_child(&mut a);
        a.push_surface(s);
        let bar = a.hint_bar(1000);
        let row = App::surface_help(s);
        for b in s.bindings() {
            let Says::Text(t) = b.says else { continue };
            assert!(bar.contains(t), "{s:?}: the bar does not say {t:?}: {bar}");
            if let Some((_, text)) = &row {
                assert!(
                    text.contains(t),
                    "{s:?}: the help row does not say {t:?}: {text}"
                );
                // The help is one wrapped screen; its row is in it.
                let first = text.split(':').next().unwrap_or(text);
                assert!(
                    help.contains(&first[..first.len().min(20)]),
                    "{s:?}: the row is not on the help screen"
                );
            }
        }
        assert!(
            bar.contains(&a.esc_hint(s)),
            "{s:?}: the way out is not named: {bar}"
        );
    }
}

/// **A chord the conversation's bar names is the chord the dispatch runs** — `ctrl-s`, `ctrl-t`,
/// `ctrl-g` and `ctrl-q` are read from `Surface::chord` by both. Pressing each opens the surface
/// the bar names it for, and the bar names it.
#[test]
fn the_bars_pane_chords_are_the_chords_that_open_those_panes() {
    let a = app();
    let bar = a.hint_bar(1000);
    for s in Surface::ALL {
        let Some((spelled, key)) = s.chord() else {
            continue;
        };
        assert!(
            bar.contains(spelled),
            "{s:?}'s chord is not on the bar: {bar}"
        );
        let mut a = app();
        a.key(key);
        assert_eq!(a.top_surface(), Some(s), "{spelled} opened something else");
    }
}

/// **The bar says what ctrl-n does** (R67 item 9's open string): it retires every note this head
/// is showing, and rano's sentence called it `ctrl-n notes` — which reads as *opens the notes*.
#[test]
fn the_conversation_bar_says_ctrl_n_clears_the_notes() {
    let a = app();
    let bar = a.hint_bar(1000);
    assert!(bar.contains("ctrl-n clears notes"), "{bar}");
    assert!(!bar.contains("ctrl-n notes"), "{bar}");
}

/// **The subagent output's Enter and the tree's Enter are told apart on the bar** — with the
/// output open over the tree the bar said the tree's "enter (or o) opens a subagent" while Enter
/// re-read the output (R67 item 9). Generated from the surface in front, the bar cannot.
#[test]
fn the_bar_follows_the_surface_in_front() {
    let mut a = app();
    running_child(&mut a);
    a.key(Key::CtrlG);
    let tree = a.hint_bar(1000);
    assert!(tree.contains("goes into it"), "{tree}");
    a.sub_out = Some(SubOut {
        session_id: "s-sub-1".into(),
        lines: vec!["out".into()],
        degraded: false,
        scroll: 0,
        spill: None,
        dropped: 0,
    });
    a.push_surface(Surface::SubOut);
    let out = a.hint_bar(1000);
    assert!(out.contains("enter reads it again"), "{out}");
    assert!(!out.contains("goes into it"), "{out}");
    assert!(out.contains("esc back to the subagents"), "{out}");
}
