//! Settings: the config pane, preferences, modes, pickers of values.

use super::*;

/// **One head's save does not discard another head's dismissals.**
///
/// The write half of the same repair, and the one that explains the operator seeing
/// MORE keys in `head.toml` than his head honoured. Both heads load once and each used
/// to write its own list back whole; `merge_retired` makes the file grow instead, so a
/// key survives a save that never heard of it.
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
    // reasons — a config row toggled, say.
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

/// `/config`: the head's rows are there at once and Enter changes one in
/// place and writes it down; the daemon's rows arrive on `Settings`, and
/// Enter on `mode` asks through the verb that already exists. A read-only
/// row says why it is.
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
    assert!(a.config_pane);
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
    // **The rung is a row too**, and it points at the verb rather than cycling (R38).
    assert!(
        screen.contains("verbosity") && screen.contains("normal"),
        "the pane does not show the rung: {screen}"
    );
    // Down to the mode row (six head rows first: diff view, verbosity, thinking, tool
    // output, raw tool calls, git format).
    for _ in 0..6 {
        a.key(Key::Down);
    }
    // The next name after `writes allowed` in the DAEMON's list — the head
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
    assert!(!a.config_pane);
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

/// **The format is the operator's, and changing it re-renders the SAME reading** — the
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
fn a_row_number_answers_the_ask_the_mode_card_and_the_quit_card() {
    // The permission ask.
    let mut a = app();
    a.apply(ServerFrame::Event(env(
        1,
        testing::requested("r1", "`bash` wants exec access"),
    )));
    assert_eq!(
        a.key(Key::Char('2')),
        Some(Action::Answer {
            req_id: "r1".into(),
            option_id: "deny".into(),
            pattern: None,
            note: None
        }),
        "row 2 of the ask is `deny`"
    );

    // The quit card.
    let mut b = app();
    b.clock(1_000);
    b.key(Key::CtrlC);
    b.key(Key::CtrlC);
    assert_eq!(b.key(Key::Char('2')), Some(Action::StopDaemon));

    // Out of range does nothing — it is not an answer and not a keystroke
    // the card invents a meaning for.
    let mut c = app();
    c.clock(1_000);
    c.key(Key::CtrlC);
    c.key(Key::CtrlC);
    assert_eq!(c.key(Key::Char('7')), None, "there is no row 7");
    assert!(c.quit_card, "and the card is still up");
}

/// **Every pane swallows the wheel, and the list is checked against the
/// panes that exist** rather than against the ones somebody remembered.
///
/// `mode_picker` arrived after the wheel router did and was added to the Esc
/// handler, the render dispatch and the footer — but not here, so a wheel in
/// the mode picker scrolled the transcript underneath and closing it left the
/// operator parked in the scrollback. That is the defect the subagent view
/// already paid for once. A test per pane, so the next one added fails here
/// rather than on somebody's screen.
#[test]
fn no_pane_lets_the_wheel_through_to_the_conversation() {
    let panes: [(&str, fn(&mut App)); 8] = [
        ("help", |a| a.help = true),
        ("picker", |a| a.picker = true),
        ("mode_picker", |a| a.pick = Some(Pick::Mode)),
        ("stats", |a| a.stats = true),
        ("todos_pane", |a| a.todos_pane = true),
        ("subagents_pane", |a| a.subagents_pane = true),
        ("jobs_pane", |a| a.jobs_pane = true),
        ("config_pane", |a| a.config_pane = true),
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

/// **`allow-all` costs one more keystroke, and no other point does.**
///
/// The operator, 2026-09-20: *"make it ask for confirmation on bare host and
/// let it thru"*. Both halves are asserted here — that it asks, and that a
/// `y` then sends the point with the consent the daemon reads.
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

    // `y` sends it, with the operator's answer on the frame.
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
    assert!(a.mode_confirm.is_none(), "the question outlived its answer");
}

/// **Enter confirms, like every other card in this head.**
///
/// It used to cancel, on the fail-closed argument that a mistyped answer
/// should be a no. Right about stray keys, wrong about the one key a person
/// presses on a confirmation — the operator read the warning, pressed it, and
/// the session stayed at `automode-edits` with only a small notice to say so.
#[test]
fn enter_confirms_the_allow_all_card_like_every_other_card() {
    for k in [Key::Enter, Key::Char('y'), Key::Char('Y')] {
        let mut a = App::new(plain_cfg(120));
        a.apply(mode_settings("always-ask", &["always-ask", "allow-all"]));
        assert_eq!(a.take_mode("allow-all".into()), None);
        let named = format!("{k:?}");
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

/// **A stray key is still a no.** The card owns the keyboard, so keystrokes
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

/// **An older daemon greens what it always greened.** No `models.keyless` row means
/// a daemon from before it existed, and the fallback is `local` alone — which is
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

/// **The real wire row, for every mode the daemon names** — and the one that had no
/// highlight at all.
///
/// The operator: *"permission mode menu no longer highlights the current mode when opened"*.
/// The cause was one word: `Mode::WRITES_ALLOWED` is spelled **`writes allowed`**, with a
/// space, and both readers of the current mode took `value.split_whitespace().next()` — so
/// `writes` named no choice, the cursor fell back to row 0, and `← now` was drawn nowhere.
/// Every other named mode was fine, which is why it looked like a bug about one screen
/// rather than about one name.
///
/// **These are the bytes the daemon actually sends**, read off `Config::settings` on this box
/// rather than retyped: `choices` is `Mode::NAMED`'s names verbatim, and one of them contains a
/// space. The fixture this file used before said `writes-allowed` — a hyphen that no mode has
/// ever had — which is exactly why the defect survived a suite that looked like it covered
/// this.
#[test]
fn every_named_mode_opens_with_its_own_row_highlighted() {
    const CHOICES: &str =
        r#"["read-only","always-ask","writes allowed","automode","automode-edits","allow-all"]"#;
    let choices: Vec<String> = serde_json::from_str(CHOICES).unwrap();
    // `(value, the row it must land on)`. The last is the consented spelling of `allow-all`,
    // whose value carries a parenthesised note — the other shape of *not a choice verbatim*.
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
        // **The two facts together, on ONE row** — where am I, and what does Enter take. The
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

/// **A name that is a prefix of another does not seed on the shorter one** — the boundary
/// half of [`named_choice`].
///
/// `automode-edits` begins with `automode`, so a bare prefix match would open the card with
/// the cursor on `automode` — a plausible-looking wrong answer, and the reason the search is
/// for the LONGEST name followed by a boundary rather than a `starts_with`.
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
    // A value that names nothing is nothing — the render then marks no row at all, which is
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
    assert!(a.picker);
    assert_eq!(
        a.command("mode"),
        Some(Action::Settings),
        "opening asks the daemon for fresh rows"
    );
    assert!(a.pick == Some(Pick::Mode));
    assert!(!a.picker, "one list on the screen at a time");
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
    // what sends it — see `allow_all_asks_before_it_is_sent_and_nothing_else_does`.
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick.is_none(), "taking a mode closes the list");
    assert_eq!(
        a.key(Key::Char('y')),
        Some(Action::Mode {
            name: "allow-all".into(),
            consented: true
        })
    );
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
    a.screen(110, 24);
    // The frame recorded where the card's first choice sat; the click's y
    // is the same 0-based coordinate.
    let first = u16::try_from(a.mode_first_row).unwrap();
    a.key(Key::Click { x: 6, y: first + 2 });
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
    // A click into the blank space under the card moves nothing: the card
    // proved six rows, and below them are its hints and the composer.
    a.key(Key::Click { x: 6, y: first + 9 });
    assert_eq!(a.mode_sel, 2);
    // Select and confirm stay two acts: the click only moves the mark.
    a.key(Key::Click { x: 6, y: first + 1 });
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
    // keys — a second cursor under it would be a cursor nothing moves —
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
    // A row number takes that row **on the keypress** — it is focus and
    // enter in one, which is what a numbered list means. See `digit_row`.
    assert_eq!(
        a.key(Key::Char('5')),
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
    // Reopened: the daemon's spelling is not the only one accepted — the
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
    // typed name is refused the same way — `/mode NAME` is still the door.
    assert_eq!(a.key(Key::Enter), None);
    let notice = a.notice.clone().unwrap();
    assert!(notice.contains("does not send the mode list"), "{notice}");
    typed(&mut a, "automode");
    assert_eq!(a.key(Key::Enter), None);
    assert!(a.pick == Some(Pick::Mode));
}

/// **A verb only its author knows is not offered** (R29), and `/diff` is new today: the
/// screen a reader actually looks at has to name it, or the remedy is one they have to go
/// looking for. The help text is one hand-kept list, so this is the check that keeps it in
/// step with the table above.
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

/// **`config_sel_line` is the line rano marks**, for every row — the layout is mirrored, and
/// this is what keeps the mirror honest.
#[test]
fn the_config_cursor_line_is_the_line_rano_marks() {
    let mut a = a_long_config(20);
    for sel in 0..a.config_rows().len() {
        a.config_sel = sel;
        let lines = a.config_lines(100);
        let at = a.config_sel_line();
        assert!(
            lines[at].trim_start().starts_with('▸'),
            "row {sel}: line {at} is {:?}",
            lines[at]
        );
    }
}

/// **Down keeps the cursor on the screen, all the way round, and comes back to the top.**
/// The operator, 2026-10-08: *"if i keep pressing down the selector eventually goes out of
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
