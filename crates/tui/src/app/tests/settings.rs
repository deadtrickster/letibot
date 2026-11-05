//! Settings: the config pane, preferences, modes, pickers of values.

use super::*;

/// One head's save does not discard another head's dismissals: both heads load once, and
/// `merge_retired` makes the file grow instead of writing its own list back whole, so a key
/// survives a save that never heard of it.
#[test]
fn a_save_keeps_a_key_this_head_never_loaded() {
    let dir = std::env::temp_dir().join(format!("letibot-union-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");

    // The other head got there first.
    crate::prefs::save(
        &path,
        &crate::prefs::HeadPrefs {
            retired: vec!["w|mode_set|1|aaaa".into()],
            ..Default::default()
        },
    )
    .unwrap();

    // This one loaded the file BEFORE that key existed, then saves for its own
    // reasons - a config row toggled, say.
    let mut a = app();
    a.prefs_path = Some(path.clone());
    a.dismissed = vec!["w|promote_idle|2|bbbb".into()];
    let _ = a.save_prefs(RetiredWrite::Union);

    let (p, _) = crate::prefs::load(&path);
    assert!(
        p.retired.iter().any(|k| k == "w|mode_set|1|aaaa"),
        "the save discarded another head's dismissal: {:?}",
        p.retired
    );
    assert!(
        p.retired.iter().any(|k| k == "w|promote_idle|2|bbbb"),
        "the save lost this head's own dismissal: {:?}",
        p.retired
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `/config`: the head's rows are there at once and Enter changes one in place and writes it
/// down; the daemon's rows arrive on `Settings`, and Enter on `mode` asks through the verb
/// that already exists. A read-only row says why it is.
#[test]
fn the_config_pane_edits_head_rows_in_place_and_persists_them() {
    let dir = std::env::temp_dir().join(format!("letibot-config-pane-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut a = app();
    a.prefs_path = Some(dir.join("head.toml"));
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));

    assert_eq!(
        a.command("config"),
        Some(Action::Settings),
        "opening asks the daemon"
    );
    assert!(a.is_open(Surface::Config));
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("diff view"), "{screen}");
    assert!(screen.contains("split"), "{screen}");
    assert!(
        screen.contains("asked the daemon; nothing back yet"),
        "{screen}"
    );

    // Row 0 is the diff view; Enter flips it and the file says so.
    assert!(a.diff_split);
    assert_eq!(a.key(Key::Enter), None);
    assert!(!a.diff_split);
    let on_disk = std::fs::read_to_string(dir.join("head.toml")).expect("head.toml written");
    assert!(on_disk.contains("diff = \"unified\""), "{on_disk}");
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("unified"), "{screen}");

    // The daemon's rows land; mode is editable through the verb.
    a.apply(ServerFrame::Settings {
        rows: vec![
            letibot_sessionlog::protocol::SettingRow {
                key: "mode".into(),
                value: "writes allowed".into(),
                source: "project store (modes.tsv)".into(),
                editable: "/mode NAME".into(),
                // The daemon's own list, which is what the pane cycles.
                choices: [
                    "read-only",
                    "always-ask",
                    "writes allowed",
                    "automode",
                    "automode-edits",
                    "allow-all",
                ]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
                tools: Vec::new(),
            },
            letibot_sessionlog::protocol::SettingRow {
                key: "oracle.budget".into(),
                value: "20.0s".into(),
                source: "--oracle-budget".into(),
                editable: String::new(),
                choices: Vec::new(),
                tools: Vec::new(),
            },
        ],
    });
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("writes allowed"), "{screen}");
    assert!(screen.contains("20.0s"), "{screen}");
    // The rung is a row too, and it points at the verb rather than cycling.
    assert!(
        screen.contains("verbosity") && screen.contains("normal"),
        "the pane does not show the rung: {screen}"
    );
    // Down to the mode row (seven head rows first: diff view, verbosity, thinking, tool
    // output, raw tool calls, todo done rows, git format).
    for _ in 0..7 {
        a.key(Key::Down);
    }
    // The next name after `writes allowed` in the DAEMON's list - the head
    // has no list of its own any more.
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Mode {
            name: "automode".into(),
            consented: false
        })
    );
    // The budget row is not editable now, and says so rather than doing nothing.
    a.key(Key::Down);
    assert_eq!(a.key(Key::Enter), None);
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("takes a restart"), "{screen}");

    if std::env::var("LETIBOT_SHOW").is_ok() {
        eprintln!("{}", a.screen(120, 30).join("\n"));
    }
    a.key(Key::Esc);
    assert!(!a.is_open(Surface::Config));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The file is read at start and applied: a head that wrote `unified`
/// yesterday draws unified today.
#[test]
fn prefs_on_disk_are_applied_at_start() {
    let dir = std::env::temp_dir().join(format!("letibot-prefs-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("head.toml");
    crate::prefs::save(
        &path,
        &crate::prefs::HeadPrefs {
            diff: crate::prefs::DiffPref::Unified,
            thinking: "open".into(),
            tools: "open".into(),
            raw_calls: true,
            verbosity: "loud".into(),
            retired: vec!["w|gate|1|0000000000000000".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let mut a = app();
    a.prefs_path = Some(path.clone());
    let (p, _) = crate::prefs::load(&path);
    a.diff_split = p.diff == crate::prefs::DiffPref::Split;
    a.reasoning = if p.thinking == "open" {
        Fold::Open
    } else {
        Fold::Folded
    };
    a.tools = if p.tools == "open" {
        Fold::Open
    } else {
        Fold::Folded
    };
    a.raw_calls = p.raw_calls;
    assert!(!a.diff_split);
    assert_eq!(a.reasoning, Fold::Open);
    assert_eq!(a.tools, Fold::Open);
    assert!(a.raw_calls);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The format is the operator's, and changing it re-renders the SAME reading - the
/// `/config` cycle through its three stops, each persisted, none of them re-reading the
/// repository.
#[test]
fn the_git_format_cycles_three_stops_and_re_renders_without_a_new_reading() {
    let mut a = app();
    a.git_state = Some(crate::gitfield::GitState {
        branch: "main".into(),
        detached: false,
        behind: Some(1),
        ahead: Some(2),
        stash: None,
        action: None,
        conflict: None,
        staged: Some(7),
        unstaged: Some(8),
        untracked: None,
    });
    a.apply_git_format();
    let said = |a: &App| -> Option<Vec<String>> {
        a.git
            .as_ref()
            .map(|p| p.iter().map(|(t, _)| t.clone()).collect())
    };
    assert_eq!(
        said(&a),
        Some(
            vec!["main", "⇣1", "⇡2", "+7", "!8"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        ),
        "the default: gitstatus's segments, in gitstatus's order"
    );
    // The cycle, three presses: default → spaced → branch alone → default.
    for (press, want) in [
        ("first", vec!["main", " !8", "+7"]),
        ("second", vec!["main"]),
        ("third", vec!["main", "⇣1", "⇡2", "+7", "!8"]),
    ] {
        a.git_format = match a.git_format.as_deref() {
            None => Some("%b %!%+".into()),
            Some("%b %!%+") => Some("%b".into()),
            _ => None,
        };
        a.apply_git_format();
        assert_eq!(
            said(&a),
            Some(want.into_iter().map(String::from).collect::<Vec<_>>()),
            "the {press} stop re-rendered the same state"
        );
    }
    // And the row names the template in force, default included.
    let rows = a.config_rows();
    let row = rows
        .iter()
        .find(|r| r.key == "git format")
        .expect("the git format row is on /config");
    assert_eq!(
        row.value,
        format!("default ({})", crate::gitfield::GIT_FORMAT_DEFAULT)
    );
}

#[test]
fn a_row_number_marks_the_ask_and_the_quit_card_and_enter_answers() {
    // The permission ask.
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::requested("r1", "`bash` wants exec access"),
    )));
    // On a decision the row number MARKS the row - the numbers are drawn - and
    // Enter answers it; the quit card's digits mark too.
    assert_eq!(a.key(Key::Char('2')), None, "the digit is a move");
    assert_eq!(a.sel, 1);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None
        }),
        "row 2 of the ask is `deny`"
    );

    // The quit card. The digit marks here too: it answered at once, so `2` typed as
    // the first character of a line stopped the daemon while the bar said "then enter".
    let mut b = app();
    b.clock(1_000);
    b.key(Key::CtrlC);
    b.key(Key::CtrlC);
    assert_eq!(b.key(Key::Char('2')), None, "the digit is a move");
    assert_eq!(b.quit_sel, 1);
    assert!(b.quit_card && !b.quit, "nothing left yet");
    assert_eq!(b.key(Key::Enter), Some(Action::StopDaemon));

    // Out of range does nothing - it is not an answer and not a keystroke
    // the card invents a meaning for.
    let mut c = app();
    c.clock(1_000);
    c.key(Key::CtrlC);
    c.key(Key::CtrlC);
    assert_eq!(c.key(Key::Char('7')), None, "there is no row 7");
    assert!(c.quit_card, "and the card is still up");
}

/// Every pane swallows the wheel, and the list is checked against the panes that exist
/// rather than the ones somebody remembered: a pane missing from it lets a notch scroll the
/// transcript underneath and leaves the operator parked in the scrollback.
#[test]
fn no_pane_lets_the_wheel_through_to_the_conversation() {
    /// One pane: the name the failure prints, and how a test opens it.
    type Pane = (&'static str, fn(&mut App));
    let panes: [Pane; 9] = [
        ("help", |a| a.push_surface(Surface::Help)),
        ("picker", |a| a.push_surface(Surface::Picker)),
        ("mode_picker", |a| a.pick = Some(Pick::Mode)),
        ("stats", |a| a.push_surface(Surface::Stats)),
        ("todos_pane", |a| a.push_surface(Surface::Todos)),
        // And the row the pane's Enter opened: the newest screen on top of the transcript, so
        // the page keys and the wheel are its chords and move `pane_scroll` - a notch here moves
        // the overlay's window and not the conversation under it.
        ("todos_open", |a| {
            a.push_surface(Surface::Todos);
            a.todos_open = Some(TodoStop::Theirs("a row".into()));
            a.push_surface(Surface::TodoRow);
        }),
        ("subagents_pane", |a| a.push_surface(Surface::Subagents)),
        ("jobs_pane", |a| a.push_surface(Surface::Jobs)),
        ("config_pane", |a| a.push_surface(Surface::Config)),
    ];
    for (name, set) in panes {
        let mut a = app();
        for i in 0..40u64 {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("u.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("u.{i}"), &format!("line {i}")),
            )));
        }
        let _ = a.screen(80, 24);
        set(&mut a);
        a.key(Key::WheelUp);
        a.key(Key::PageUp);
        assert_eq!(a.scroll, 0, "{name} let the wheel reach the conversation");
    }
}

/// `allow-all` costs one more keystroke, and no other point does: it asks first, and a `y`
/// then sends the point with the consent the daemon reads.
#[test]
fn allow_all_asks_before_it_is_sent_and_nothing_else_does() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings(
        "always-ask",
        &["always-ask", "automode", "allow-all"],
    ));

    // An ordinary point goes straight out, unchanged.
    assert_eq!(
        a.take_mode("automode".into()),
        Some(Action::Mode {
            name: "automode".into(),
            consented: false
        })
    );

    // `allow-all` does not. It emits nothing and puts the question on screen.
    assert_eq!(a.take_mode("allow-all".into()), None, "sent without asking");
    let screen = a.screen(120, 30).join("\n");
    assert!(screen.contains("privilege escalation"), "{screen}");
    assert!(screen.contains("[y] or [enter] confirm"), "{screen}");

    // `y` sends it, with the operator's answer on the frame - past the card's arming
    // window, which is the guard for keys that were on their way before it appeared.
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
    assert!(a.mode_confirm.is_none(), "the question outlived its answer");
}

/// Enter confirms, like every other card in this head: it does not cancel on the fail-closed
/// argument that a mistyped answer should be a no, which is right about stray keys and wrong
/// about the one key a person presses on a confirmation.
#[test]
fn enter_confirms_the_allow_all_card_like_every_other_card() {
    for k in [Key::Enter, Key::Char('y'), Key::Char('Y')] {
        let mut a = App::new(plain_cfg(120));
        a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
        assert_eq!(a.take_mode("allow-all".into()), None);
        let named = format!("{k:?}");
        a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
        assert_eq!(
            a.key(k),
            Some(Action::Mode {
                name: "allow-all".into(),
                consented: true
            }),
            "{named} did not confirm"
        );
        assert!(a.mode_confirm.is_none(), "{named} left the question up");
    }
    // And the card names exactly the keys it takes.
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
    a.take_mode("allow-all".into());
    let line = a.mode_confirm_line().expect("a question");
    assert!(line.contains("[y] or [enter] confirm"), "{line}");
}

/// A stray key is still a no. The card owns the keyboard, so keystrokes
/// aimed at the composer land on it, and those must not be consent.
#[test]
fn a_stray_key_cancels_the_allow_all_confirmation() {
    for k in [Key::Esc, Key::Char('n'), Key::Char('a')] {
        let mut a = App::new(plain_cfg(120));
        a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
        assert_eq!(a.take_mode("allow-all".into()), None);
        let named = format!("{k:?}");
        assert_eq!(a.key(k), None, "{named} sent the mode");
        assert!(a.mode_confirm.is_none(), "{named} left the question up");
    }
}

/// The pointer answers nothing on the allow-all card: a wheel step or a click is not a
/// keystroke aimed at the composer, so neither cancels - scrolling up to reread what
/// allow-all means must not say `allow-all cancelled`. `y` still confirms after them.
#[test]
fn the_wheel_and_a_click_leave_the_allow_all_card_standing() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
    assert_eq!(a.take_mode("allow-all".into()), None);
    for k in [Key::WheelUp, Key::WheelDown, Key::Click { x: 3, y: 2 }] {
        let named = format!("{k:?}");
        assert_eq!(a.key(k), None, "{named} sent the mode");
        assert!(a.mode_confirm.is_some(), "{named} answered the question");
    }
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
}

/// The card opens asynchronously, from another head's frame, so the Enter that was on its way
/// can be the keystroke that meant to send the line the operator was typing. The card's first
/// moments take no answer and say so; past them, Enter is the deliberate key it always was.
#[test]
fn the_cards_first_moments_take_no_enter() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
    a.session_id = "s-1".into();
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-1".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    assert_eq!(
        a.key(Key::Enter),
        None,
        "enter on the card's heels answered it"
    );
    assert!(a.mode_confirm.is_some(), "the card did not stay up");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("just came up")),
        "the refusal must say what to do"
    );
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::ModeConsent {
            req_id: "h2:req-1".into(),
            yes: true
        })
    );
}

/// Enter keeps the quit card's guard: words in the composer are a line being written, and the card
/// takes no answer until they are gone. The guard is asked below `sync_front`, because the hold is
/// what makes it unreachable from the keys.
#[test]
fn enter_over_words_in_the_composer_does_not_answer_the_card() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
    assert_eq!(a.take_mode("allow-all".into()), None);
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    a.set_composer("one more thing");
    assert!(
        matches!(
            a.key_mode_confirm(&Key::Enter),
            std::ops::ControlFlow::Break(None)
        ),
        "enter over words answered the card"
    );
    assert!(a.mode_confirm.is_some(), "the card did not stay up");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("words in it"))
    );
    a.set_composer("");
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
}

/// A card that arrives over a half-typed line holds it, as a question does, and gives it back
/// when the card is answered: the Enter that meant to send the line must never confirm allow-all.
/// The card arrives as a frame - another head's ask - which is the door that finds words typed.
#[test]
fn the_card_holds_the_line_the_operator_was_typing() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
    a.session_id = "s-1".into();
    typed(&mut a, "one more thing");
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-1".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    assert_eq!(
        a.held_line.as_deref(),
        Some("one more thing"),
        "the line went with the card"
    );
    assert!(a.editor.text().is_empty());
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('n')),
        Some(Action::ModeConsent {
            req_id: "h2:req-1".into(),
            yes: false
        })
    );
    assert!(a.mode_confirm.is_none());
    assert_eq!(
        a.editor.text(),
        "one more thing",
        "the held line did not come back with the card's answer"
    );
}

/// A second confirmation must not silently replace the first: the card on the screen owns the
/// keys, so the request behind it waits, and takes the slot when the front one is answered.
#[test]
fn a_second_confirmation_waits_behind_the_card_in_front() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings(
        "writes allowed",
        &["writes allowed", "allow-all"],
    ));
    a.session_id = "s-1".into();
    for req in ["h2:req-1", "h2:req-2"] {
        a.apply(ServerFrame::ModeConfirm {
            session_id: "s-1".into(),
            req_id: req.into(),
            name: "allow-all".into(),
            asked_by: "mcp".into(),
        });
    }
    assert_eq!(
        a.mode_asked.as_ref().map(|m| m.req_id.as_str()),
        Some("h2:req-1"),
        "the second request replaced the card the screen was showing"
    );
    assert_eq!(a.mode_waiting.len(), 1, "the second request was dropped");
    assert!(
        a.notice
            .as_deref()
            .is_some_and(|n| n.contains("waiting behind"))
    );

    // Answering the front card settles ITS id, and the waiting one takes the slot.
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('n')),
        Some(Action::ModeConsent {
            req_id: "h2:req-1".into(),
            yes: false
        })
    );
    assert_eq!(
        a.mode_asked.as_ref().map(|m| m.req_id.as_str()),
        Some("h2:req-2"),
        "the waiting request did not come up"
    );
    assert!(a.mode_confirm.is_some());

    // And it answers by its own id, past its own arming window.
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::ModeConsent {
            req_id: "h2:req-2".into(),
            yes: true
        })
    );
    assert!(a.mode_confirm.is_none() && a.mode_asked.is_none());
}

/// A confirmation the DAEMON raised - another head asked for `allow-all` - is the same card,
/// with two differences: it says who asked, and its answer is `Action::ModeConsent`, a frame
/// carrying the request's id, because the daemon has to settle the request it raised.
#[test]
fn a_confirmation_another_head_asked_for_is_the_same_card_answered_by_its_id() {
    let mut a = App::new(plain_cfg(120));
    a.apply(mode_settings(
        "writes allowed",
        &["writes allowed", "allow-all"],
    ));
    a.session_id = "s-1".into();
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-1".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    // The same warning, and the asking head named - a card a machine raised must not read as
    // the person's own keystroke.
    let screen = a.screen(120, 40).join("\n");
    assert!(screen.contains("privilege escalation"), "{screen}");
    assert!(
        screen.contains("requested by `mcp`") || screen.contains("`mcp`"),
        "the card does not say who asked: {screen}"
    );

    // `y` answers THE REQUEST, not a point the head invented - past the arming window,
    // which holds the card's first moments against keys pressed before it appeared.
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::ModeConsent {
            req_id: "h2:req-1".into(),
            yes: true
        })
    );
    assert!(a.mode_confirm.is_none() && a.mode_asked.is_none());

    // A `no` is an answer too, and it is sent: a person declining is a fact the daemon has
    // to record, or the request stays open. Esc is not that key - it puts the card away and
    // sends nothing, and `n` is the refusal.
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-2".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(a.key(Key::Esc), None, "esc sent a refusal");
    assert!(a.mode_confirm.is_none() && a.mode_away.is_some());
    a.key(Key::AltA);
    assert!(
        a.mode_confirm.is_some(),
        "alt-a did not bring the card back"
    );
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('n')),
        Some(Action::ModeConsent {
            req_id: "h2:req-2".into(),
            yes: false
        })
    );

    // The settlement takes the card away, whoever answered. Another head may have said yes
    // first (the daemon settles with whoever answers), and a card left up would own the keyboard
    // for a request nobody can settle.
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-3".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    a.apply(ServerFrame::ModeSettled {
        session_id: "s-1".into(),
        req_id: "h2:req-3".into(),
        client_request_id: String::new(),
        name: "allow-all (this box, consented)".into(),
        applied: true,
        answer: "yes".into(),
        said: "dead@box confirmed it".into(),
    });
    assert!(a.mode_confirm.is_none(), "the card outlived its request");
    assert!(a.mode_asked.is_none());

    // And a settlement for a request this head is not holding leaves it alone.
    a.apply(ServerFrame::ModeConfirm {
        session_id: "s-1".into(),
        req_id: "h2:req-4".into(),
        name: "allow-all".into(),
        asked_by: "mcp".into(),
    });
    a.apply(ServerFrame::ModeSettled {
        session_id: "s-1".into(),
        req_id: "h2:somebody-elses".into(),
        client_request_id: String::new(),
        name: "allow-all".into(),
        applied: false,
        answer: "no".into(),
        said: "nothing changed".into(),
    });
    assert!(
        a.mode_confirm.is_some(),
        "another head's card was dismissed"
    );

    // The operator's own `/mode allow-all` while that card is up is still their keystroke,
    // and it must not settle somebody else's request: the answered card is theirs.
    a.take_mode("allow-all".into());
    assert!(
        a.mode_asked.is_none(),
        "the daemon's request rode the operator's card"
    );
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
}

/// An older daemon greens what it always greened. No `models.keyless` row means
/// a daemon from before it existed, and the fallback is `local` alone - which is
/// precisely the behaviour this head had before, so nothing reads as newly broken.
#[test]
fn without_the_keyless_row_local_is_still_ready_and_nothing_else_new_is() {
    use letibot_sessionlog::protocol::{MODEL_KEYS_KEY, SettingRow};
    let mut a = App::new(RenderConfig {
        width: 110,
        color: true,
        ..RenderConfig::default()
    });
    a.settings = vec![SettingRow {
        key: MODEL_KEYS_KEY.into(),
        value: "deepseek".into(),
        source: String::new(),
        editable: String::new(),
        choices: Vec::new(),
        tools: Vec::new(),
    }];
    assert!(a.choice_ready("local"));
    assert!(a.choice_ready("deepseek/deepseek-flash"));
    assert!(!a.choice_ready("dense78"));
}

/// The mode picker seeds on the row the daemon actually sends: `Mode::WRITES_ALLOWED` is
/// spelled `writes allowed`, so a reader taking `split_whitespace().next()` found no choice
/// and fell back to row 0 - which is why the fixture uses `Mode::NAMED`'s bytes verbatim.
#[test]
fn every_named_mode_opens_with_its_own_row_highlighted() {
    const CHOICES: &str =
        r#"["read-only","always-ask","writes allowed","automode","automode-edits","allow-all"]"#;
    let choices: Vec<String> = serde_json::from_str(CHOICES).unwrap();
    // `(value, the row it must land on)`. The last is the consented spelling of `allow-all`,
    // whose value carries a parenthesised note - the other shape of *not a choice verbatim*.
    for (value, at) in [
        ("read-only", 0usize),
        ("always-ask", 1),
        ("writes allowed", 2),
        ("automode", 3),
        ("automode-edits", 4),
        ("allow-all", 5),
        ("allow-all (this box, consented)", 5),
    ] {
        let mut a = app();
        a.apply(hello(
            "s",
            vec![brief("s", "one", false)],
            Hub::new("s").snapshot(),
        ));
        a.apply(ServerFrame::Settings {
            rows: vec![letibot_sessionlog::protocol::SettingRow {
                key: "mode".into(),
                value: value.into(),
                source: "project store (modes.tsv)".into(),
                editable: "/mode NAME".into(),
                choices: choices.clone(),
                tools: Vec::new(),
            }],
        });
        a.command("mode");
        assert_eq!(
            a.mode_sel, at,
            "`{value}` opened with the cursor on the wrong row"
        );
        let screen = a.screen(110, 30);
        let row = screen
            .iter()
            .find(|l| l.contains('\u{25b8}') && l.contains("  "))
            .expect("a marked row");
        // The two facts together, on ONE row - where am I, and what does Enter take. The
        // defect broke both, and asserting either alone would have passed for one of them.
        assert!(
            row.contains('\u{2190}') && row.contains("now"),
            "`{value}` is not marked as the current mode: {row:?}"
        );
        assert_eq!(
            screen.iter().filter(|l| l.contains('\u{2190}')).count(),
            1,
            "`{value}` marked more than one row as current"
        );
    }
}

/// A name that is a prefix of another does not seed on the shorter one: `automode-edits`
/// begins with `automode`, so a bare prefix match opens the card on a plausible-looking
/// wrong answer - hence the LONGEST name followed by a boundary, not a `starts_with`.
#[test]
fn a_mode_whose_name_extends_another_does_not_seed_on_the_shorter_one() {
    let choices: Vec<String> = vec!["automode".into(), "automode-edits".into()];
    assert_eq!(
        named_choice("automode-edits", &choices),
        Some("automode-edits")
    );
    assert_eq!(named_choice("automode", &choices), Some("automode"));
    // And the qualifier is still read through: the consented spelling of the shorter name.
    assert_eq!(
        named_choice("automode (this box, consented)", &choices),
        Some("automode")
    );
    // A value that names nothing is nothing - the render then marks no row at all, which is
    // the honest answer for a current value the daemon did not offer as a choice.
    assert_eq!(named_choice("something-else", &choices), None);
}

#[test]
fn bare_mode_opens_the_picker_seeded_to_the_current_mode() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("always-ask", MODES));
    // The two pickers are never open together: each opener closes the other.
    a.key(Key::CtrlS);
    assert!(a.is_open(Surface::Picker));
    assert_eq!(
        a.command("mode"),
        Some(Action::Settings),
        "opening asks the daemon for fresh rows"
    );
    assert!(a.pick == Some(Pick::Mode));
    assert!(
        !a.is_open(Surface::Picker),
        "one list on the screen at a time"
    );
    assert_eq!(
        a.mode_sel, 1,
        "the cursor starts on the mode the session is under"
    );
    let screen = a.screen(110, 24);
    let card = screen
        .iter()
        .position(|l| l.contains("the mode this session runs under"))
        .expect("the card is on the screen");
    // A bottom card, not a body panel: the session header and the
    // transcript's own rows are still above it, the way the ask card sits.
    let header = screen
        .iter()
        .position(|l| l.contains("1/1"))
        .expect("the session header is still drawn");
    assert!(header < card, "transcript above, card below:\n{screen:?}");
    let row = screen
        .iter()
        .find(|l| l.contains("always-ask") && l.contains('▸'))
        .expect("the current mode is on the screen");
    assert!(
        row.contains("← now"),
        "the row says which mode is live: {row}"
    );
    // Enter on the untouched list is a no-op that closes: the session is
    // already in the marked mode, and a round trip to be told what the
    // screen already showed is not worth its flicker.
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick.is_none());
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("already that mode"), "{notice}");
}

#[test]
fn the_mode_picker_moves_with_arrows_and_enter_takes_the_marked_row() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    assert_eq!(a.mode_sel, 0);
    a.key(Key::Down);
    a.key(Key::Down);
    assert_eq!(a.mode_sel, 2);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("writes allowed") && l.contains('▸'))
        .unwrap();
    assert!(row.contains('▸'), "the mark moved with the arrows: {row}");
    // Up wraps past the top; Up again wraps in from the bottom.
    a.key(Key::Up);
    a.key(Key::Up);
    assert_eq!(a.mode_sel, 0);
    a.key(Key::Up);
    assert_eq!(a.mode_sel, 5);
    // `allow-all` is the one point the head holds for a confirmation, so Enter
    // closes the list and puts the question up instead of sending it. `y` is
    // what sends it - see `allow_all_asks_before_it_is_sent_and_nothing_else_does`.
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick.is_none(), "taking a mode closes the list");
    a.now_ms += crate::app::keys::MODE_ARM_MS + 1;
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
}

/// A row number marks the row and Enter answers it, the same contract the decision ladder
/// and the quit card keep: a digit is also the first character of a line somebody is typing.
#[test]
fn a_digit_marks_and_enter_answers_on_the_setting_card() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    assert_eq!(a.mode_sel, 0, "the cursor is seeded to the current mode");
    assert_eq!(a.key(Key::Char('2')), None, "a digit answers nothing");
    assert_eq!(a.mode_sel, 1, "it moved the mark");
    assert!(a.pick == Some(Pick::Mode), "the card stays up until Enter");
    // Enter takes the marked row: `always-ask` is an ordinary point and goes straight out.
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Mode {
            name: "always-ask".into(),
            consented: false
        })
    );
    assert!(a.pick.is_none());
}

/// The setting card answers no key while another card hides it: a question in front takes the
/// digit as its own answer, and the mode the card holds must not change behind the question.
#[test]
fn a_hidden_setting_card_takes_no_digit() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    // A question lands over the card: the card is not drawn, and its keys are not its own.
    a.apply(ServerFrame::Event(env(
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    )));
    assert!(
        a.hint_bar(200).contains("↑↓ marks"),
        "the bar names the card in front: {}",
        a.hint_bar(200)
    );
    assert_eq!(a.key(Key::Char('2')), None);
    assert_eq!(a.mode_sel, 0, "the hidden card's cursor moved");
    assert_eq!(
        a.editor.text(),
        "2",
        "the digit did not reach the question's answer"
    );
    assert!(a.pick == Some(Pick::Mode), "the card itself was closed");
}

/// The picker's keys are its own exactly while the slot draws it: the keys and the drawing read
/// one rule, and this checks it against the frame rather than against a second copy of the arms.
#[test]
fn the_setting_card_is_keyed_exactly_while_the_slot_draws_it() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    let title = "the mode this session runs under";
    let drawn = |a: &mut App| a.screen(110, 30).iter().any(|l| l.contains(title));
    assert!(
        drawn(&mut a) && a.pick_card_up(),
        "alone, it is drawn and keyed"
    );
    // The two cards that can take the slot over a picker: a question, and the new-todo card.
    for front in ["q1", "todo"] {
        if front == "q1" {
            a.apply(ServerFrame::Event(env(
                1,
                testing::asked("q1", "which branch?", &["main", "release"], ""),
            )));
        } else {
            assert_eq!(a.command("todo"), None, "bare /todo opens the card");
        }
        assert!(
            !drawn(&mut a) && !a.pick_card_up(),
            "under {front} the card is neither drawn nor keyed"
        );
        a.key(Key::Esc);
    }
    assert!(
        drawn(&mut a) && a.pick_card_up(),
        "and it comes back with the slot"
    );
}

/// A click recorded on the setting card lands on nothing once another card has taken the slot:
/// its rows were measured from a frame whose card is no longer the one in front.
#[test]
fn a_click_on_the_setting_card_lands_on_nothing_once_a_decision_is_up() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    let frame = a.screen(110, 24);
    let y = value_row(&frame, 3, MODES[2]);
    a.apply(ServerFrame::Event(env(
        1,
        testing::asked("q1", "which branch?", &["main", "release"], ""),
    )));
    assert_eq!(a.key(Key::Click { x: 6, y }), None);
    assert_eq!(a.mode_sel, 0, "the hidden card's cursor moved");
    assert_eq!(a.sel, 0, "the click took the question's ladder");
}

/// The screen row a setting card's value `n` (1-based, as the card numbers it) was drawn on,
/// found on the frame - never counted, which is how the click arithmetic went wrong.
fn value_row(frame: &[String], n: usize, name: &str) -> u16 {
    let label = format!("{n}  {name}");
    frame
        .iter()
        .position(|l| l.contains(&label))
        .unwrap_or_else(|| panic!("{label:?} is drawn: {frame:#?}")) as u16
}

#[test]
fn a_click_on_the_mode_card_picks_the_row_under_the_pointer() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    let frame = a.screen(110, 24);
    a.key(Key::Click {
        x: 6,
        y: value_row(&frame, 3, MODES[2]),
    });
    assert_eq!(a.mode_sel, 2);
    let row = a
        .screen(110, 24)
        .into_iter()
        .find(|l| l.contains("writes allowed") && l.contains('▸'))
        .unwrap();
    assert!(
        row.contains('▸'),
        "the mark moved to the clicked row: {row}"
    );
    // A click on the card's keys line, under the values, moves nothing.
    let keys = frame
        .iter()
        .position(|l| l.contains("esc leaves it alone"))
        .unwrap() as u16;
    a.key(Key::Click { x: 6, y: keys });
    assert_eq!(a.mode_sel, 2);
    // Select and confirm stay two acts: the click only moves the mark.
    a.key(Key::Click {
        x: 6,
        y: value_row(&frame, 2, MODES[1]),
    });
    assert_eq!(a.mode_sel, 1);
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Mode {
            name: "always-ask".into(),
            consented: false
        })
    );
    assert!(a.pick.is_none());
}

/// A click on the setting card is measured from where the card was drawn.
/// The card's first row was taken from the top of the whole chrome, so every line pushed above
/// it - the daemon link line here - shifted every click by that many rows.
#[test]
fn a_click_on_the_mode_card_lands_under_a_link_line() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    a.command("mode");
    a.link_down("the daemon closed the connection");
    let frame = a.screen(110, 30);
    assert!(
        frame.iter().any(|l| l.contains("connection is down")),
        "{frame:#?}"
    );
    for (k, name) in MODES.iter().enumerate() {
        a.key(Key::Click {
            x: 6,
            y: value_row(&frame, k + 1, name),
        });
        assert_eq!(a.mode_sel, k, "a click on {name}'s row");
    }
}

/// Every value of every setting card is clickable, its wrapped meaning included.
/// `/verbosity` carries a `why` under each value, wrapped, so one row is not one value - and the
/// count of clickable rows was the MODE row's choices whatever the card was about.
#[test]
fn a_click_on_the_verbosity_card_picks_the_value_drawn_there() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    a.command("verbosity");
    let names: Vec<String> = a.pick_values().into_iter().map(|(v, _)| v).collect();
    assert!(names.len() > 2);
    let frame = a.screen(110, 60);
    for (k, name) in names.iter().enumerate().rev() {
        let y = value_row(&frame, k + 1, name);
        a.key(Key::Click { x: 6, y });
        assert_eq!(a.mode_sel, k, "a click on {name}'s row");
        // The line under it is its meaning, and picks it too.
        a.mode_sel = usize::MAX;
        a.key(Key::Click { x: 12, y: y + 1 });
        assert_eq!(a.mode_sel, k, "a click on {name}'s meaning");
    }
}

#[test]
fn the_mode_card_steps_aside_while_a_decision_is_up() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    assert!(
        a.screen(110, 24)
            .iter()
            .any(|l| l.contains("the mode this session runs under"))
    );
    // A permission ask arrives: it takes the card slot and the ladder
    // keys - a second cursor under it would be a cursor nothing moves  -
    // and the mode card waits, then comes back once it is answered.
    a.apply(ServerFrame::Event(env(1, testing::requested("r1", "rm"))));
    assert!(
        !a.screen(110, 24)
            .iter()
            .any(|l| l.contains("the mode this session runs under"))
    );
    assert!(
        a.pick == Some(Pick::Mode),
        "the card waits, it does not close"
    );
    a.apply(ServerFrame::Event(env(2, testing::answered("r1", "deny"))));
    assert!(
        a.screen(110, 24)
            .iter()
            .any(|l| l.contains("the mode this session runs under"))
    );
}

#[test]
fn the_mode_picker_takes_a_row_number_a_name_and_refuses_an_ambiguous_prefix() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    // A row number MARKS that row and Enter takes it - an answer on the digit itself was the
    // first character of a line somebody was typing. See `digit_row`.
    assert_eq!(a.key(Key::Char('5')), None);
    assert_eq!(a.mode_sel, 4, "the digit moved the mark");
    assert!(a.pick == Some(Pick::Mode), "and answered nothing");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Mode {
            name: "automode-edits".into(),
            consented: false
        })
    );
    assert!(a.pick.is_none());
    // Reopened: an exact name wins even though a longer mode starts with it.
    assert_eq!(a.command("mode"), Some(Action::Settings));
    typed(&mut a, "automode");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Mode {
            name: "automode".into(),
            consented: false
        })
    );
    // Reopened: the daemon's spelling is not the only one accepted - the
    // fold is `Mode::parse`'s own, so what `/mode NAME` takes the list takes.
    assert_eq!(a.command("mode"), Some(Action::Settings));
    typed(&mut a, "automode_edits");
    assert_eq!(
        a.key(Key::Enter),
        Some(Action::Mode {
            name: "automode-edits".into(),
            consented: false
        })
    );
    // Reopened: an ambiguous prefix is refused with a count, list still up.
    assert_eq!(a.command("mode"), Some(Action::Settings));
    typed(&mut a, "auto");
    assert_eq!(a.key(Key::Enter), None);
    assert!(
        a.pick == Some(Pick::Mode),
        "an ambiguous prefix leaves the list up"
    );
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("2 modes match"), "{notice}");
    // A prefix nothing matches is refused the same way.
    typed(&mut a, "nope");
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick == Some(Pick::Mode));
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("no mode matches"), "{notice}");
}

#[test]
fn mode_with_a_name_still_goes_straight_to_the_daemon() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(mode_settings("read-only", MODES));
    assert_eq!(
        a.command("mode automode-edits"),
        Some(Action::Mode {
            name: "automode-edits".into(),
            consented: false
        }),
        "a named mode never opens the list"
    );
    assert!(a.pick.is_none());
}

#[test]
fn a_mode_picker_without_choices_says_so() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    // A daemon older than protocol 18 sends no choices; the head keeps no
    // list of its own to fall back on.
    a.apply(mode_settings("read-only", &[]));
    assert_eq!(a.command("mode"), Some(Action::Settings));
    assert!(a.pick == Some(Pick::Mode));
    let screen = a.screen(110, 24).join("\n");
    assert!(screen.contains("has not named its modes"), "{screen}");
    // Enter says so rather than falling through to the composer, and a
    // typed name is refused the same way - `/mode NAME` is still the door.
    assert_eq!(a.key(Key::Enter), None);
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("does not send the mode list"), "{notice}");
    typed(&mut a, "automode");
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick == Some(Pick::Mode));
}

/// A verb only its author knows is not offered: the help text is one hand-kept list, so this
/// is the check that keeps it in step with the table above - the screen a reader looks at has
/// to name the remedy.
#[test]
fn the_help_screen_names_both_settings_and_what_a_bare_one_does() {
    let a = app();
    let screen = help_lines(&a.cfg, 140).join("\n");
    assert!(screen.contains("/verbosity"), "{screen}");
    assert!(screen.contains("/diff"), "{screen}");
    let line = screen
        .lines()
        .position(|l| l.contains("/diff"))
        .expect("`/diff` is on the help screen");
    assert!(
        screen.lines().nth(line).unwrap_or("").contains("unified")
            || screen.lines().nth(line + 1).unwrap_or("").contains("split"),
        "the help screen names `/diff` without saying what it takes:\n{screen}"
    );
}

/// A config pane with `n` daemon rows under the head's own, so it outgrows a short terminal.
fn a_long_config(n: usize) -> App {
    let mut a = App::new(plain_cfg(100));
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(a.command("config"), Some(Action::Settings));
    a.apply(ServerFrame::Settings {
        rows: (0..n)
            .map(|i| letibot_sessionlog::protocol::SettingRow {
                key: format!("setting.{i:02}"),
                value: format!("value {i}"),
                source: "a flag".into(),
                editable: String::new(),
                choices: Vec::new(),
                tools: Vec::new(),
            })
            .collect(),
    });
    a
}

/// Down keeps the cursor on the screen, all the way round, and comes back to the top.
/// The operator: *"if i keep pressing down the selector eventually goes out of
/// view and doesnt wrap … until i reenter config pane"*.
#[test]
fn down_keeps_the_config_cursor_in_view_and_wraps_to_the_first_row() {
    let mut a = a_long_config(30);
    let n = a.config_rows().len();
    a.screen(100, 16);
    for press in 1..=n {
        a.key(Key::Down);
        let screen = a.screen(100, 16).join("\n");
        assert!(
            screen.contains('▸'),
            "press {press}: the cursor is off the screen:\n{screen}"
        );
    }
    assert_eq!(a.config_sel, 0, "n presses go round once");
    let screen = a.screen(100, 16).join("\n");
    assert!(
        screen.contains("config"),
        "back at the top, the pane's title shows:\n{screen}"
    );
    // And Up from the first row goes to the last, still on screen.
    a.key(Key::Up);
    assert_eq!(a.config_sel, n - 1);
    assert!(a.screen(100, 16).join("\n").contains('▸'));
}

/// The stood-down compaction arrives on `Settings`, and only in the guard's own spelling:
/// a head that attaches after the guard fired never saw the warning, and a bare `off` is the
/// operator's own `--no-auto-compact`, which is not news to them.
#[test]
fn the_stood_down_row_sets_and_clears_the_resident_line() {
    use letibot_sessionlog::protocol::SettingRow;
    let mut a = app();
    let stood_row = |value: &str| SettingRow {
        key: "auto-compact".into(),
        value: value.into(),
        source: String::new(),
        editable: String::new(),
        choices: Vec::new(),
        tools: Vec::new(),
    };
    // The guard's finding, as the daemon renders it: the `off - ` spelling,
    // with the pair.
    a.apply(ServerFrame::Settings {
        rows: vec![stood_row(
            "off — the no-progress guard stood it down: compacted 1849499 to 1530411 \
             tokens and still no room for the next turn",
        )],
    });
    let line = a
        .auto_compact_off
        .as_deref()
        .expect("the row set the state");
    assert!(
        line.contains("no room"),
        "the line carries the daemon's own why, not a paraphrase: {line}"
    );
    // A bare `off` - the operator's own flag - is not the guard's finding and
    // must not draw the line.
    a.apply(ServerFrame::Settings {
        rows: vec![stood_row("off")],
    });
    assert!(
        a.auto_compact_off.is_none(),
        "an off the operator typed is not re-announced to them"
    );
    // And `on` clears it: the line is a state, and states end.
    a.apply(ServerFrame::Settings {
        rows: vec![stood_row(
            "off — the no-progress guard stood it down: compacted 1849499 to 1530411 \
             tokens and still no room for the next turn",
        )],
    });
    assert!(a.auto_compact_off.is_some());
    a.apply(ServerFrame::Settings {
        rows: vec![stood_row("on")],
    });
    assert!(
        a.auto_compact_off.is_none(),
        "a session the guard never fired on takes the line away"
    );
}

/// A paused plan-check is LOUD: the `nags` row's `paused` spelling draws a resident line
/// above the composer naming the end, `nags_resumed` takes it away, and `on` is no line -
/// on both channels, the row for a later attach and the hub rows for a watcher.
#[test]
fn a_paused_plan_check_draws_a_resident_line_until_it_is_resumed() {
    use letibot_sessionlog::protocol::SettingRow;
    let nags_row = |value: &str| SettingRow {
        key: "nags".into(),
        value: value.into(),
        source: String::new(),
        editable: String::new(),
        choices: Vec::new(),
        tools: Vec::new(),
    };
    let mut a = app();
    a.apply(ServerFrame::Settings {
        rows: vec![nags_row("on")],
    });
    assert!(a.nags_paused.is_none());
    assert!(
        !a.screen(120, 30).join("\n").contains("nags PAUSED"),
        "a running clock draws nothing"
    );

    a.apply(ServerFrame::Settings {
        rows: vec![nags_row("paused until 14:30")],
    });
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("nags PAUSED until 14:30") && screen.contains("/nag resume"),
        "the resident line names the end and the verb that ends it: {screen}"
    );
    // On every frame, not once: a second draw still carries it.
    assert!(a.screen(120, 30).join("\n").contains("nags PAUSED"));

    // `/status` carries it as a state row too.
    a.command("status");
    let status = a.screen(120, 60).join("\n");
    assert!(status.contains("PAUSED until 14:30"), "{status}");

    // The live channel: the daemon's `nags_resumed` row clears it at once.
    let hub = letibot_sessionlog::hub::Hub::new("s");
    let e = hub.publish(letibot_sessionlog::SessionEvent::Warning {
        code: "nags_resumed".into(),
        detail: "nags RESUMED: the pause ran out at 14:30".into(),
        compaction: None,
    });
    a.apply(ServerFrame::Event(e));
    assert!(a.nags_paused.is_none(), "a resume takes the line away");

    // And `nags_paused` draws it before the settings name its end - and asks for them.
    let e = hub.publish(letibot_sessionlog::SessionEvent::Warning {
        code: "nags_paused".into(),
        detail: "nags PAUSED by the operator until you resume them".into(),
        compaction: None,
    });
    a.queued.clear();
    a.apply(ServerFrame::Event(e));
    assert!(a.nags_paused.is_some(), "the pause is drawn at once");
    assert!(
        a.queued.contains(&Action::Settings),
        "and the settings are asked for, so the line can name the end"
    );
}

/// A paused session is marked in the picker, from its brief's `nags_paused` - the session
/// list is where a silent board on ANOTHER session would otherwise be read as a broken one.
#[test]
fn the_picker_marks_a_paused_session() {
    let mut paused = brief("s-quiet", "the quiet one", false);
    paused.nags_paused = Some(letibot_sessionlog::NagPause::UntilResumed);
    let other = brief("s-loud", "the loud one", false);
    let mut a = app();
    a.apply(hello(
        "s-loud",
        vec![other, paused],
        letibot_sessionlog::hub::Hub::new("s-loud").snapshot(),
    ));
    a.push_surface(Surface::Picker);
    let lines = a.picker_lines(96).join("\n");
    assert!(
        lines.contains("the quiet one · nags paused"),
        "the paused session says so on its row: {lines}"
    );
    assert!(
        !lines.contains("the loud one · nags paused"),
        "and only that one: {lines}"
    );
}

/// The todos pane's `n` is `/nag pause`, and `/nag resume` while paused - declared in the
/// binding table, so the hint bar names it.
#[test]
fn n_in_the_todos_pane_pauses_and_resumes_the_plan_checks() {
    let mut a = app();
    a.session_id = "s1".into();
    a.push_surface(Surface::Todos);
    assert_eq!(
        a.key(Key::Char('n')),
        Some(Action::Slash {
            line: "nag pause".into()
        })
    );
    a.nags_paused = Some("until 14:30".into());
    assert_eq!(
        a.key(Key::Char('n')),
        Some(Action::Slash {
            line: "nag resume".into()
        })
    );
    let bar = a.surface_hint(Surface::Todos);
    assert!(
        bar.contains("pauses or resumes the plan-checks"),
        "the bar names the key: {bar}"
    );
}

/// The todos pane's `d` is a row here too - every switch this head keeps is on `/config`, and
/// the row and the pane's bar read the same flag, so the two cannot disagree about which way it
/// stands. Enter goes through the pane's own toggle, which is what writes the file.
#[test]
fn the_config_pane_shows_and_flips_the_todo_done_switch() {
    let dir = std::env::temp_dir().join(format!("letibot-config-hide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut a = app();
    a.prefs_path = Some(dir.join("head.toml"));
    a.push_surface(Surface::Config);

    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("todo done rows") && screen.contains("folded"),
        "the switch is not a row of the pane, or it does not show where a head starts: {screen}"
    );

    // The sixth of the head's rows: diff view, verbosity, thinking, tool output, raw tool calls,
    // and then this one.
    a.config_sel = 5;
    a.key(Key::Enter);
    assert!(!a.todos_fold_done, "Enter did not flip the switch");
    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("todo done rows") && screen.contains("shown"),
        "the row does not show the new state: {screen}"
    );
    let on_disk = std::fs::read_to_string(dir.join("head.toml")).expect("head.toml written");
    assert!(
        on_disk.contains("todos_hide_done = false"),
        "the flip was not written down: {on_disk}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A settings frame nobody asked for is drawn: the daemon pushes the rows when one of them
/// changes, so a head that is not the one that moved the mode reads the new value off the frame
/// rather than off a photograph taken at attach - and sends nothing to be told.
#[test]
fn a_settings_frame_nobody_asked_for_is_drawn() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", true)],
        Hub::new("s").snapshot(),
    ));
    assert_eq!(
        a.command("config"),
        Some(Action::Settings),
        "opening asks once"
    );
    a.apply(mode_settings("read-only", MODES));
    assert!(
        a.screen(120, 30).join("\n").contains("read-only"),
        "the premise: the pane is drawing the row it read"
    );
    a.take_actions();

    // Another head moved the mode, and the daemon pushed the rows to this one too.
    a.apply(mode_settings("writes allowed", MODES));

    let screen = a.screen(120, 30).join("\n");
    assert!(
        screen.contains("writes allowed"),
        "the pane is still drawing the mode it read at attach: {screen}"
    );
    assert_eq!(a.mode_current(), "writes allowed");
    assert!(
        a.take_actions().is_empty(),
        "the head asked again for rows it was just handed"
    );
}

/// The same walk over `Surface::ALL`, so a surface added later is held to it without anybody
/// adding it to the list above.
#[test]
fn no_surface_lets_the_wheel_through_to_the_conversation() {
    for s in Surface::ALL {
        let mut a = app();
        for i in 0..40u64 {
            a.apply(ServerFrame::Event(env(
                i * 2 + 1,
                testing::appended(&format!("u.{i}"), "user"),
            )));
            a.apply(ServerFrame::Event(env(
                i * 2 + 2,
                testing::content(&format!("u.{i}"), &format!("line {i}")),
            )));
        }
        let _ = a.screen(80, 24);
        a.push_surface(s);
        a.key(Key::WheelUp);
        a.key(Key::PageUp);
        assert_eq!(a.scroll, 0, "{s:?} let the wheel reach the conversation");
    }
}
