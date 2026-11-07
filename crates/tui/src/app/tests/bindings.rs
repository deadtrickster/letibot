//! Each surface's keys are declared once, and the hint bar and `/help` are that table
//! read as words - so a binding appears in both, a plain letter never acts on a typed line, and
//! a chord the bar names is the chord the dispatch runs.

use super::*;
use crate::app::keys::bindings::{Act, Guard, On, Says};

/// A plain letter always carries the empty-composer guard - the letters are the operator's
/// to type first. The queue pane's `a`/`v`/`d`/`r` had no guard where the subagents pane's
/// `o`/`p` did; the table makes the rule checkable for every surface at once.
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

/// And a word typed with any surface open reaches the composer - the behaviour the guard is
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
        // FIRST letter on an empty composer is a key - that is what a pane letter is.)
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

/// The bar and the help are the table read as words: every binding that says something is on the
/// bar while its surface is in front and in the help row for its surface. Generated, so a binding
/// added later with words cannot be missing from either.
#[test]
fn every_binding_that_says_something_is_on_the_bar_and_in_the_help() {
    let head = app();
    let help = help_lines(&head.cfg, 400, &head.key_overrides).join("\n");
    for s in Surface::ALL {
        let mut a = app();
        a.session_id = "s1".into();
        running_child(&mut a);
        a.push_surface(s);
        let bar = a.hint_bar(1000);
        let row = a.surface_help(s);
        for b in s.bindings() {
            // What each binding says on the bar, by the route the bar takes: a `Text` says itself
            // and a `Switch` asks the head which way it stands - so one the head has no words for
            // is a binding the bar silently drops, which is the drift this test catches.
            let says = match b.says {
                Says::Text(t) => t.to_string(),
                Says::Switch(help) => {
                    // The bar says the act that would happen now; the HELP says both directions,
                    // because it is read without a list in front of it.
                    if let Some((_, text)) = &row {
                        assert!(
                            text.contains(help),
                            "{s:?}: the help row does not say {help:?}: {text}"
                        );
                    }
                    a.switch_says(b)
                        .unwrap_or_else(|| panic!("{s:?}: no words for {b:?}"))
                }
                Says::Row | Says::Nothing => continue,
            };
            assert!(
                bar.contains(&says),
                "{s:?}: the bar does not say {says:?}: {bar}"
            );
            if let Some((_, text)) = &row {
                if matches!(b.says, Says::Text(_)) {
                    assert!(
                        text.contains(says.as_str()),
                        "{s:?}: the help row does not say {says:?}: {text}"
                    );
                }
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

/// A chord the conversation's bar names is the chord the dispatch runs - `ctrl-s`, `ctrl-t`,
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

/// The bar says what ctrl-n does: it retires every note this head
/// is showing, and rano's sentence called it `ctrl-n notes` - which reads as *opens the notes*.
#[test]
fn the_conversation_bar_says_ctrl_n_clears_the_notes() {
    let a = app();
    let bar = a.hint_bar(1000);
    assert!(bar.contains("ctrl-n clears notes"), "{bar}");
    assert!(!bar.contains("ctrl-n notes"), "{bar}");
}

/// The subagent output's Enter and the tree's Enter are told apart on the bar - with the
/// output open over the tree the bar said the tree's "enter (or o) opens a subagent" while Enter
/// re-read the output. Generated from the surface in front, the bar cannot.
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

/// The `Says` literals on the conversation bar have no dispatch table behind them, so each is
/// driven here: the key named does what the words say. A new literal fails until it is added.
#[test]
fn every_says_literal_on_the_conversation_bar_does_what_it_says() {
    use crate::ui::hint_bar::{CONVERSATION, Chord};
    for c in CONVERSATION {
        let Chord::Says(key, says) = c else { continue };
        let mut a = app();
        a.apply(hello(
            "s",
            vec![brief("s", "one", false)],
            Hub::new("s").snapshot(),
        ));
        match (*key, *says) {
            ("ctrl-n", "clears notes") => {
                a.key(Key::CtrlN);
                let by_key = a.notice.clone();
                let mut b = app();
                let _ = b.notes_command("notes", "dismiss all");
                assert_eq!(by_key, b.notice, "ctrl-n is not `/notes dismiss all`");
            }
            ("ctrl-v", "picture or newest result") => {
                a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
                let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
                a_result_row(&mut a, 2, "i1", &long);
                a.key(Key::CtrlV);
                assert_eq!(a.payload_sel.as_deref(), Some("i1"), "no result window");
            }
            ("/t", "all tool rows") => {
                assert_eq!(a.command("t"), None);
                assert!(a.tools.is_open(), "/t did not unfold the tool rows");
            }
            ("alt-n", "newest notice") => {
                a.key(Key::AltN);
                assert!(
                    a.notice.as_deref().is_some_and(|n| n.contains("no notice")),
                    "alt-n did not go for the newest notice: {:?}",
                    a.notice
                );
            }
            ("ctrl-p", "hold") => {
                a.key(Key::CtrlP);
                assert!(a.hold, "ctrl-p did not hold the view");
            }
            ("/help", "") => {
                let _ = a.command("help");
                assert_eq!(a.top_surface(), Some(Surface::Help));
            }
            other => panic!("the bar names {other:?} and no test drives it"),
        }
    }
}

// ---- K9: `[keys]` in `leticode.toml` ----

/// One `[keys]` line, as the reader hands it over: the name, the key, and the line it was on.
fn rebind(name: &str, key: &str) -> leticode_config::Rebind {
    leticode_config::Rebind {
        name: name.into(),
        key: key.into(),
        from: letibot_tools::exec::boundary::Origin {
            file: std::path::PathBuf::from("/tmp/leticode.toml"),
            line: 7,
            user: false,
        },
    }
}

/// K9's done-test: a rebound key is honoured - the new key reaches the binding, the old one does
/// not, and the bar and the help say the new one - and a key the head claims GLOBALLY is not
/// rebindable, which is W8's rule: a binding is rebound within its surface, never the escape.
#[test]
fn a_rebound_key_is_honoured_and_a_global_chord_is_not_rebindable() {
    let keys = leticode_config::KeyKeys {
        rebind: vec![rebind("todos.d", "f")],
    };
    let (overrides, said) = KeyOverrides::resolve(&keys);
    assert!(said.is_empty(), "a well-formed line was refused: {said:?}");

    let mut a = app();
    a.session_id = "s1".into();
    running_child(&mut a);
    a.key_overrides = overrides;
    a.push_surface(Surface::Todos);

    // The new key reaches what `d` did, and `d` no longer does.
    let moved = a
        .binding_for(Surface::Todos, &Key::Char('f'))
        .expect("`f` is not bound on the todos pane after the rebinding");
    assert_eq!(moved.act, Act::FoldDone, "`f` reaches another act");
    assert!(
        !a.binding_for(Surface::Todos, &Key::Char('d'))
            .is_some_and(|b| b.act == Act::FoldDone),
        "the old key still reaches the binding it was moved off"
    );

    // And nothing advertises the old key: the bar's words for the move are `f`'s, and the help's
    // are too - an advertisement of a press that does nothing is the defect W8 names. The two
    // screens ask the binding different questions (the bar says what a press would do NOW, the
    // help says both directions of a switch), so each is read for what it writes.
    let now = a
        .switch_says(&moved)
        .expect("the binding still says something");
    let bar = a.surface_hint(Surface::Todos);
    assert!(bar.contains(&format!("f {now}")), "{bar}");
    assert!(!bar.contains(&format!("d {now}")), "{bar}");
    let both = match moved.says {
        Says::Text(t) | Says::Switch(t) => t,
        Says::Row | Says::Nothing => panic!("the moved binding has no words: {moved:?}"),
    };
    let (_, help) = a
        .surface_help(Surface::Todos)
        .expect("the pane is in the help");
    assert!(help.contains(&format!("f {both}")), "{help}");
    assert!(!help.contains(&format!("d {both}")), "{help}");

    // A global chord, as the target and as the source: both refused, with the line.
    for (name, key) in [
        ("todos.w", "ctrl-c"),
        ("todos.w", "esc"),
        ("todos.ctrl-r", "x"),
    ] {
        let (overrides, said) = KeyOverrides::resolve(&leticode_config::KeyKeys {
            rebind: vec![rebind(name, key)],
        });
        assert!(
            overrides.moved(Surface::Todos, On::Char('w')) == On::Char('w'),
            "{name} = {key} was applied as well as refused"
        );
        assert_eq!(said.len(), 1, "{name} = {key}: {said:?}");
        assert!(
            said[0].contains("/tmp/leticode.toml:7:"),
            "the refusal does not name the file and the line: {}",
            said[0]
        );
        assert!(
            said[0].contains("global"),
            "the refusal does not say why: {}",
            said[0]
        );
    }

    // A surface or a key this head has not got is refused by name, with the line.
    for (name, key, want) in [
        ("nowhere.d", "f", "names no surface"),
        ("todos.z", "f", "is not a key of the todos"),
        ("todos.d", "zz", "is not a key this head spells"),
        ("todos.d", "n", "already taken on the todos"),
    ] {
        let (_, said) = KeyOverrides::resolve(&leticode_config::KeyKeys {
            rebind: vec![rebind(name, key)],
        });
        assert_eq!(said.len(), 1, "{name} = {key}: {said:?}");
        assert!(
            said[0].starts_with("/tmp/leticode.toml:7:"),
            "the refusal does not name the file and the line: {}",
            said[0]
        );
        assert!(said[0].contains(want), "{name} = {key}: {}", said[0]);
    }

    // And with no `[keys]` at all, nothing moves: the shipped head is the table as written.
    let (none, said) = KeyOverrides::resolve(&leticode_config::KeyKeys::default());
    assert!(said.is_empty() && none.moved(Surface::Todos, On::Char('d')) == On::Char('d'));
    assert_eq!(
        none.moved(Surface::Todos, On::Char('d')),
        On::Char('d'),
        "the default override moves a key"
    );
}

/// The refusal list is the head's own chords, and the half of it a table can prove is proved here:
/// every chord a surface advertises and every chord a switch names is a word the rebinding refuses.
/// The rest (esc, ctrl-c, and the chords with no table to read) are arms of `key_chords` and
/// `key_dispatch`, so they are the list's own word - stated once, in [`GLOBAL_CHORDS`].
#[test]
fn the_global_chords_are_the_ones_the_tables_advertise() {
    use crate::app::keys::bindings::{GLOBAL_CHORDS, Word, word_of};
    let advertised = Surface::ALL
        .iter()
        .filter_map(|s| s.chord())
        .chain(Show::ALL.iter().filter_map(|s| s.chord()));
    for (name, _) in advertised {
        assert!(
            GLOBAL_CHORDS.contains(&name),
            "the head claims {name} whatever is open and `[keys]` would let a surface take it"
        );
    }
    // Every word on the list is a word the reader knows, so a rebind onto one is a refusal that
    // says WHY rather than "not a key".
    for c in GLOBAL_CHORDS {
        assert!(
            matches!(word_of(c), Word::Global),
            "`{c}` is on the refusal list and its own parser does not recognise it"
        );
    }
}
