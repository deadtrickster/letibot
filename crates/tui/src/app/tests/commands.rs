//! Slash commands and the dispatcher.

use super::*;

/// **Ctrl+C Ctrl+C asks which exit.** The operator, 2026-09-17: *"when i do
/// CcCc i should be asked if I want to exit letibot or letibot and
/// harnessd"*.
///
/// The card defaults to the cheap answer, because the other one takes the
/// daemon's KV with it and a cold prefill of a long session is minutes on
/// this box — a default that costs that much is a default that has answered
/// for the operator.
/// **A row number is focus and enter, on every card that draws one.** The
/// operator, 2026-09-17: *"it shows numbered lists anyway so me pressing row
/// number should constitute focus and enter"*.
///
/// One rule, three cards, and the same guard on each: the composer must be
/// empty, so a line already being typed keeps its digits.
/// **The door's verbs are typed with hyphens** — R34.
///
/// The operator: *"lets change /web_search to /web-search - no shift needed."* The door's
/// names were the only underscored verbs in either registry, and they were underscored
/// because they are spelled straight from the TOOL names. Right for the wire, wrong for a
/// keyboard.
///
/// Three assertions, and they are the three clauses of the requirement: the hyphen form is
/// what is OFFERED, the underscore form is ACCEPTED, and **the wire still carries the
/// tool's own name** — this is a textual transform and not knowledge about the tool.
#[test]
fn a_door_verb_is_typed_with_hyphens() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(door_row("web_search", "query", "text"));

    // **Offered hyphenated.** Tab from `/web-` reaches it.
    typed(&mut a, "/web-");
    a.key(Key::Tab);
    assert_eq!(a.input(), "/web-search");

    // **And the underscore form is accepted rather than refused** — an operator who
    // types what the daemon calls it should not be told they are wrong.
    assert_eq!(
        a.command("web-search blabla"),
        Some(Action::HeadRun {
            name: "web_search".into(),
            arguments: r#"{"query":"blabla"}"#.into(),
        }),
        "the hyphen form sends the TOOL's name"
    );
    assert_eq!(
        a.command("web_search blabla"),
        Some(Action::HeadRun {
            name: "web_search".into(),
            arguments: r#"{"query":"blabla"}"#.into(),
        }),
        "and so does the underscore form: `/web-search` and `/web_search` are one verb"
    );
    // Case is a hand's, not a meaning's.
    assert_eq!(
        a.command("Web-Search blabla"),
        Some(Action::HeadRun {
            name: "web_search".into(),
            arguments: r#"{"query":"blabla"}"#.into(),
        })
    );
}

/// **The head holds no schema: the arguments come off the row.**
///
/// Four facts, and each is a different way for the head to be right without knowing what
/// the tool is: the field the line goes into, the defaults that travel with it, the
/// no-bare-form refusal in the daemon's own words, and the JSON form kept for a tool with
/// several arguments.
#[test]
fn the_bare_form_is_built_from_the_row_and_nothing_else() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(door_row("read", "path", "path"));
    assert_eq!(
        a.command("read crates/tui/src/app.rs"),
        Some(Action::HeadRun {
            name: "read".into(),
            arguments: r#"{"path":"crates/tui/src/app.rs"}"#.into(),
        })
    );

    // **A default travels with the line**, so a bare form sends what a model's minimal
    // call would have sent.
    let mut b = app();
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let mut row = door_row("web_fetch", "url", "url");
    if let ServerFrame::Settings { rows } = &mut row {
        rows[0].tools[0]
            .defaults
            .insert("format".into(), "markdown".into());
    }
    b.apply(row);
    // **Compared as a VALUE, not as a string.** Key order in a JSON object is not a fact
    // about the call, and an assertion on it would fail the day the map's iteration
    // changes while the call stayed the same.
    let Some(Action::HeadRun { name, arguments }) = b.command("web-fetch http://example.invalid")
    else {
        panic!("the bare form did not produce a door call");
    };
    assert_eq!(name, "web_fetch");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments).expect("an object"),
        serde_json::json!({"url": "http://example.invalid", "format": "markdown"}),
        "the field the line went into, and the default that travelled with it"
    );

    // **No bare form: the daemon's own sentence, not the head's guess.**
    //
    // Called on the function rather than through `command`, and the reason is worth
    // stating: the door's three tools ALL have a single required field today, so this
    // branch is unreachable through a real row — it exists for an allowlist entry with two
    // required fields, and the sentence it returns is the daemon's `why_json`. A test that
    // could not reach it would be no test at all.
    let mut c = app();
    c.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    let two_fields = letibot_sessionlog::HeadRunTool {
        name: "pair".into(),
        field: String::new(),
        kind: String::new(),
        defaults: Default::default(),
        why_json: "`pair` needs 2 fields (a, b)".into(),
    };
    let why = c
        .head_run_call(&two_fields, "a b")
        .expect_err("a tool with no bare form is refused");
    assert_eq!(
        why, "`pair` needs 2 fields (a, b)",
        "the daemon's own sentence"
    );
    // And a bare verb with nothing after it is refused by name, with the field named —
    // a call to a tool that needs a query, with no query, is a call nobody meant.
    let read = letibot_sessionlog::HeadRunTool {
        name: "read".into(),
        field: "path".into(),
        kind: "path".into(),
        defaults: Default::default(),
        why_json: String::new(),
    };
    let why = c.head_run_call(&read, "   ").expect_err("no path, no call");
    assert!(
        why.contains("/read WHAT") && why.contains("`path`"),
        "{why}"
    );
    assert!(why.contains("path"), "the kind is named: {why}");

    // **And the JSON form stays**, which is what a tool with several arguments needs.
    let Some(Action::HeadRun { arguments, .. }) = a.command(r#"read {"path":"x","limit":3}"#)
    else {
        panic!("the JSON form was not accepted");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments).expect("an object"),
        serde_json::json!({"path": "x", "limit": 3}),
        "the JSON form goes through untouched — checked for BEING json and nothing else"
    );
    // **The JSON form is entered by the brace, and only by it.** A line with spaces is
    // ONE bare argument, which is how a path with a space in it works; a line that opens
    // with `{` is the operator asking for the JSON form and is parsed as one.
    let Some(Action::HeadRun { arguments, .. }) = a.command("read x y") else {
        panic!("a line with spaces is one bare path");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments).unwrap(),
        serde_json::json!({"path": "x y"})
    );
    assert_eq!(
        a.command("read {not json"),
        None,
        "an opening brace promises JSON; a malformed one is refused, not read as a path"
    );
    assert!(
        a.notice.as_deref().is_some_and(|n| n.contains("read")),
        "and the refusal names the verb: {:?}",
        a.notice
    );
    // **And the object is passed through unexamined** — checked for BEING json and for
    // nothing else, which is R24's own rule kept: the head does not know `path` is
    // required, so it does not pretend to, and the tool's own refusal is the one that
    // reaches the operator.
    let Some(Action::HeadRun { arguments, .. }) = a.command(r#"read {"limit":3}"#) else {
        panic!("an object is accepted whatever is in it");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments).unwrap(),
        serde_json::json!({"limit": 3})
    );
}

/// **The door's list is the daemon's allowlist, not the row.** A row naming a tool the
/// daemon does not admit is not a door verb — the row describes what is admitted, and a
/// head that trusted a row over the list would be offering a call the daemon refuses.
///
/// This is the failure that taught me: the first version of the test above used a
/// two-required-field tool called `pair` and expected the door to refuse it. It did not —
/// `pair` is not in `HEAD_RUN_TOOLS`, so it fell through to the daemon as an ordinary
/// unknown verb, which is exactly right.
#[test]
fn a_row_naming_a_tool_outside_the_allowlist_is_not_a_door_verb() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(door_row("bash", "command", "text"));
    assert!(
        matches!(a.command("bash rm -rf /"), Some(Action::Slash { .. })),
        "the door does not admit `bash` and the row cannot make it"
    );
}

/// **A verb the head has no tool for is not swallowed by the door.** The door arm runs
/// first in `command`, so a name it does not match must fall through to everything else
/// — otherwise every verb in the registry would be refused by a door that never heard of
/// it.
#[test]
fn a_verb_that_is_not_a_door_tool_falls_through() {
    let mut a = app();
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(door_row("read", "path", "path"));
    // A daemon verb still goes to the daemon.
    assert!(matches!(
        a.command("gate recent"),
        Some(Action::Slash { .. })
    ));
    // And a head verb still does what it did.
    assert_eq!(a.command("t"), None);
    assert!(a.tools.is_open());
    // A door name with no row is not a door: nothing was published, so nothing is
    // matched, and it goes to the daemon like any other unknown word.
    assert!(matches!(
        a.command("web-search blabla"),
        Some(Action::Slash { .. })
    ));
}

/// **The completion table and the dispatcher are not two lists that agree by
/// maintenance** (R32).
///
/// In Rust a `match` is not reflectable, so of the two mechanisms the requirement
/// allows — one derived from the other, or a test that fails when they diverge — this is
/// the second. It reads the source of [`App::command`] and names every verb in it, so an
/// arm added without a row in [`SLASH_COMMANDS`] fails here rather than becoming a verb
/// nobody is ever offered.
///
/// **The finding it pins, measured on this tree 2026-09-23**
/// (`docs/evidence/slash-completion-2026-09-23.py`): the table offered 27 verbs and the
/// dispatcher acted on 38, and the gap was not one oversight but the shape — a registry
/// that exists for one purpose read as the answer to a different question.
///
/// `HEAD_COMMAND_ALIASES` are excluded on purpose: the table's own comment says offering
/// both spellings doubles the list to teach the same actions, and `command()` keeps
/// taking them.
#[test]
fn every_verb_the_dispatcher_acts_on_is_offered_by_tab() {
    // The dispatcher's own source, so this cannot drift from the code it is about.
    const SRC: &str = include_str!("../commands.rs");
    let start = SRC
        .find("fn command(&mut self, cmd: &str) -> Option<Action> {")
        .expect("the dispatcher");
    let mut depth = 0usize;
    let mut end = start;
    for (i, c) in SRC[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &SRC[start..end];

    // **Plain string surgery, no regex**: this crate has no regex dependency and
    // adding one to a test would be a dependency for the test's convenience.
    let mut acted: Vec<String> = Vec::new();
    let quoted = |hay: &str| -> Vec<String> {
        hay.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    };
    // `verb_arg(cmd, "x")` — the leading-position verbs that take arguments.
    for (i, _) in body.match_indices("verb_arg(cmd, \"") {
        let rest = &body[i + "verb_arg(cmd, \"".len()..];
        if let Some(q) = rest.find('"') {
            acted.push(rest[..q].to_string());
        }
    }
    // `matches!(cmd, "a" | "b")`.
    for (i, _) in body.match_indices("matches!(cmd, ") {
        let rest = &body[i + "matches!(cmd, ".len()..];
        let close = rest.find(')').expect("a matches! has a close");
        acted.extend(quoted(&rest[..close]));
    }
    // `cmd.strip_prefix("switch ")`.
    for (i, _) in body.match_indices("cmd.strip_prefix(\"") {
        let rest = &body[i + "cmd.strip_prefix(\"".len()..];
        if let Some(q) = rest.find('"') {
            acted.push(rest[..q].trim().to_string());
        }
    }
    // The `match cmd` arms: a run of `"a" | "b"` before a `=>`, **at the match's own
    // depth**. Two guards, and the second is the one that matters: a string literal
    // inside an arm's BODY also begins a trimmed line with a quote, and a first draft of
    // this test reported seven verbs including `self.say`'s prose — a check that greps
    // for nearly the right thing reads exactly like a check that greps for the right
    // one, which is this document's oldest note.
    let mi = body.find("        match cmd {").expect("the match");
    let mut d = 0i32;
    for line in body[mi..].split('\n') {
        let t = line.trim();
        if d == 1 && t.starts_with('"') && t.contains("=>") {
            let head = t.split("=>").next().unwrap_or("");
            acted.extend(quoted(head));
        }
        d += line.matches('{').count() as i32 - line.matches('}').count() as i32;
    }
    assert!(
        acted.len() > 20,
        "the extraction found {} verbs",
        acted.len()
    );

    let offered: Vec<&str> = SLASH_COMMANDS.iter().map(|(n, _)| *n).collect();
    let mut missing: Vec<&String> = acted
        .iter()
        .filter(|v| {
            !offered.contains(&v.as_str())
                    && !HEAD_COMMAND_ALIASES.contains(&v.as_str())
                    // A `reseat` modifier is the same verb: the table carries the base and
                    // the one modifier worth a line of its own.
                    && !v.starts_with("reseat")
        })
        .collect();
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "/help and Tab cannot offer a verb the head acts on: {missing:?}\n\
             Add a row to SLASH_COMMANDS (or to HEAD_COMMAND_ALIASES if it is a spelling of \
             another verb). This is the gap `slash-completion-2026-09-23.py` measured."
    );
}

#[test]
fn the_facts_and_the_keys_are_on_their_own_rows_not_in_the_field() {
    let mut a = app();
    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    let screen = a.screen(100, 20);
    let (row, _) = a.cursor().unwrap();
    let joined = screen.join("\n");
    // The top edge closes the box and says nothing: the legend that lived
    // there — model, dialect, endpoint, verbosity — was a row of attention
    // the eye paid on every return to the field for facts read once.
    assert!(screen[row - 1].contains('╭'), "{:?}", screen[row - 1]);
    assert!(!screen[row - 1].contains("normal"), "{:?}", screen[row - 1]);
    // …the bottom edge closes the box and says nothing, because nothing has
    // gone wrong…
    assert!(screen[row + 1].contains('╰'), "{:?}", screen[row + 1]);
    assert!(
        !screen[row + 1].contains("seq"),
        "the telemetry is not resident in the operator's frame: {:?}",
        screen[row + 1]
    );
    // …and the keys on a bar below the box, never in the field.
    assert!(screen[row + 2].contains("ctrl-r"), "{:?}", screen[row + 2]);
    assert!(!screen[row].contains("ctrl-r"), "{:?}", screen[row]);
    assert!(!joined.contains("dropped 0"), "{joined}");
    // Reachable in one command, with the sequence numbers, the full id, and
    // the verbosity the border used to carry.
    a.command("status");
    let stats = a.screen(100, 40).join("\n");
    assert!(stats.contains("dropped"), "{stats}");
    assert!(stats.contains("seq"), "{stats}");
    assert!(stats.contains("verbosity"), "{stats}");
    assert!(stats.contains("normal"), "{stats}");
}

/// **§7 C14: the completion table lists what this head implements.**
///
/// The ruling is that the table is the UNION of both heads' verbs and wants one
/// shared artefact — a cross-tree change, filed rather than half-done. What can be
/// checked here is the half that is this head's: **every verb in the table is one
/// this head answers**, and the five §6 verbs plus `/models` and `/resync` are on it.
/// A table naming a verb the head refuses is the allowlist defect with the sign
/// flipped — the head advertising something it does not have.
#[test]
fn every_listed_slash_command_is_one_this_head_answers() {
    for (name, hint) in SLASH_COMMANDS {
        // **A stated contract, checked rather than assumed**: the head documents
        // itself with this table, so an entry that does nothing is `/help` lying.
        // "Something happened" is either an action for the driver or a line said
        // into the notice — and the composer is untouched either way, because a
        // command that half-typed itself would be the worst of both.
        let mut a = app();
        a.session_id = "s1".into();
        // **The observable for a pane toggle is the pane.** `ctrl-p` and `/subagents`
        // return `None` and say nothing on the way out, because a screen appearing or
        // disappearing is its own feedback — so "something happened" is an action, a
        // line said, **or a pane changing state**, and the last is checked by diffing
        // the flags rather than by trusting the return value. My first version of this
        // assertion assumed every verb either asks the daemon or speaks, and
        // `/subagents` does neither, which is correct behaviour and a wrong test.
        let panes = |a: &App| {
            (
                a.help,
                a.stats,
                a.picker,
                a.todos_pane,
                a.subagents_pane,
                a.jobs_pane,
                a.config_pane,
                a.pick == Some(Pick::Mode),
                a.pick == Some(Pick::Model),
                // **R38 added two settings to the same card**, so a verb that opens one
                // is a verb that did something — and this closure is where "something"
                // is defined for the self-documentation check.
                a.pick == Some(Pick::Verbosity),
                a.pick == Some(Pick::Diff),
                a.slash_out.is_some(),
            )
        };
        let before = panes(&a);
        let action = a.submit(format!("/{name}"));
        assert!(
            action.is_some() || a.notice.is_some() || panes(&a) != before,
            "/{name} ({hint}) is listed and did nothing — no action, no notice, \
                 no pane changed"
        );
        assert_eq!(a.input(), "", "/{name} left the composer dirty");
        assert!(
            a.notice.is_none() || !a.notice.as_deref().unwrap_or("").contains("unknown"),
            "/{name} is listed and reported as unknown"
        );
    }
    // And the seven this commit added are really on it.
    for want in [
        "todos",
        "subagents",
        "peek",
        "resume",
        "promote",
        "models",
        "resync",
    ] {
        assert!(
            SLASH_COMMANDS.iter().any(|(n, _)| *n == want),
            "/{want} is missing from the completion table"
        );
    }
}

#[test]
fn a_path_is_shortened_at_a_separator_and_a_pattern_is_not() {
    assert_eq!(
        ellipsise_left("/home/dead/Projects/letibot/crates/tui", 22),
        "…/letibot/crates/tui",
        "whole segments, and the longest suffix that fits"
    );
    assert_eq!(
        shorten_subject("crates/tui/src/app.rs", 14),
        "…/src/app.rs",
        "a path loses its left"
    );
    assert_eq!(
        shorten_subject("^pub (fn|struct|enum)", 12),
        "^pub (fn|st…",
        "a pattern loses its right — it is read from the start"
    );
    assert_eq!(
        shorten_subject("**/*.{md,json,toml,yaml}", 12),
        "**/*.{md,js…",
        "and so does a glob, slash or no slash"
    );
    assert_eq!(
        shorten_subject("\"what is src/main.rs for\"", 12),
        "\"what is sr…",
        "a quoted sentence is prose with a slash in it, not a path"
    );
    // A single segment with no separator to cut on falls back to characters
    // rather than returning something wider than it was asked for.
    assert!(rano::width::text::width(&ellipsise_left("averylongsinglesegment", 10)) <= 10);
}

/// With a name after it the line goes over as typed — `--once` and `--key`
/// have to survive, and they are the reason bare-vs-argument is the split.
#[test]
fn models_with_an_argument_still_goes_straight_to_the_daemon() {
    let mut a = app();
    a.session_id = "s1".into();
    for line in [
        "models deepseek/deepseek-flash",
        "models grok --key xai-test",
    ] {
        match a.command(line) {
            Some(Action::Slash { line: sent }) => assert_eq!(sent, line),
            other => panic!("{line}: {other:?}"),
        }
        assert!(a.pick.is_none(), "{line} is not a menu");
    }
}

/// **The shell path leaves plain and `/` lines alone, and the slash path
/// leaves `!` lines alone** — each completes only its own sigil, so a line
/// is never completed by the wrong one.
#[test]
fn the_shell_path_leaves_plain_and_slash_lines_alone_and_vice_versa() {
    let mut a = app();
    shell_row(
        &mut a,
        1,
        "u1",
        "user",
        TranscriptItem::User {
            speaker: letibot_transcript::Speaker::Operator,
            parts: vec![UserPart::Text {
                text: "! ls -la".into(),
            }],
        },
    );
    // A plain prompt line is untouched by the shell path.
    typed(&mut a, "hello");
    a.key(Key::Tab);
    assert_eq!(a.input(), "hello", "a plain line is untouched");
    // A `/` line is untouched by the shell path (it is the slash path's).
    a.set_composer("/se");
    a.completion = None;
    a.key(Key::Tab);
    assert_eq!(a.input(), "/sessions", "a / line goes to the slash path");
    // A `!` line is untouched by the slash path.
    a.set_composer("! ls");
    a.completion = None;
    a.key(Key::Tab);
    assert_eq!(a.input(), "! ls -la", "a ! line goes to the shell path");
}

/// **A job's output opens a screen; it does not land in the conversation.**
///
/// `/job ID` can hand back 16 KiB of whatever a command wrote, and it arrives
/// as `Warning { code: "slash" }` like every other slash reply — so a head
/// that renders warnings as notes put a subprocess's stdout, ANSI and all,
/// into the scrollback between the model's turns. The operator: *"it returned
/// the output in the main conversation window wtf"*.
#[test]
fn a_long_slash_reply_opens_a_pane_and_a_short_one_stays_a_note() {
    let mut a = App::new(plain_cfg(80));
    let notes_before = a.notes.len();
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "slash".into(),
            detail: "/job j89\nline one\nline two\nline three\nline four".into(),

            compaction: None,
        },
    )));
    assert!(
        a.slash_out.is_some(),
        "a listing must not go to the scrollback"
    );
    assert_eq!(a.notes.len(), notes_before, "and must not also be a note");
    let screen = a.screen(80, 24).join("\n");
    assert!(screen.contains("/job j89"), "{screen}");
    assert!(screen.contains("line four"), "{screen}");

    // Esc closes it and the conversation is back.
    a.key(Key::Esc);
    assert!(a.slash_out.is_none());

    // A one-line confirmation is still a note: a screen for it would be a
    // keystroke to dismiss nothing.
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::Warning {
            code: "slash".into(),
            detail: "/mode automode\nthis session is at `automode`".into(),

            compaction: None,
        },
    )));
    assert!(a.slash_out.is_none(), "a sentence is not a listing");
    assert!(a.notes.len() > notes_before);
}

/// **A subprocess's control bytes never reach the terminal.**
///
/// What `/job` returns is whatever the command wrote — colour, cursor moves,
/// a scroll region. Rendered straight, those are instructions to the
/// operator's terminal from a process nobody vetted.
#[test]
fn a_slash_listing_strips_control_characters() {
    let mut a = App::new(plain_cfg(80));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Warning {
            code: "slash".into(),
            detail: "/job j1\n\u{1b}[2m dim \u{1b}[0m\nb\nc\nd".into(),

            compaction: None,
        },
    )));
    let (_, lines) = a.slash_out.clone().expect("a listing");
    assert!(
        !lines.iter().any(|l| l.contains('\u{1b}')),
        "an escape survived into the pane: {lines:?}"
    );
    assert!(
        lines[0].contains("dim"),
        "the text itself is kept: {lines:?}"
    );
}

#[test]
fn ctrl_q_and_slash_jobs_toggle_the_pane_and_esc_closes_it() {
    let mut a = App::new(plain_cfg(80));
    // Opening asks the daemon for its table; closing asks nothing.
    assert_eq!(a.key(Key::CtrlQ), Some(Action::ListJobs));
    assert!(a.jobs_pane);
    assert_eq!(a.key(Key::CtrlQ), None);
    assert!(!a.jobs_pane);

    assert_eq!(a.command("jobs"), Some(Action::ListJobs));
    assert!(a.jobs_pane);
    a.key(Key::Esc);
    assert!(!a.jobs_pane, "esc closes the pane like the other screens");
}

/// **A verb has to be the whole word.** The operator: *"/models doesnt work —
/// printed `mode ls` requested lol"*. `strip_prefix("mode")` took `/models` as
/// `/mode` with the argument `ls`, and returned before the fallthrough that
/// forwards a daemon verb — so `/models` never left the head.
#[test]
fn a_command_that_merely_starts_like_a_verb_is_not_that_verb() {
    assert_eq!(verb_arg("mode", "mode"), Some(""));
    assert_eq!(verb_arg("mode automode", "mode"), Some("automode"));
    assert_eq!(verb_arg("models", "mode"), None, "the reported one");
    assert_eq!(
        verb_arg("newton", "new"),
        None,
        "not a session called `ton`"
    );
    assert_eq!(verb_arg("renamed", "rename"), None);
    assert_eq!(verb_arg("cellsomething", "cells"), None);
}

/// And the whole-word rule holds: `/models` is no longer swallowed by the
/// `mode` arm, which took it as `/mode` with the argument `ls`. Bare it opens
/// the picker; with a name it goes to the daemon.
#[test]
fn slash_models_reaches_the_daemon() {
    let mut a = app();
    a.session_id = "s1".into();
    assert_eq!(a.command("models"), Some(Action::Settings));
    assert!(a.pick == Some(Pick::Model), "bare /models is the menu");
    a.key(Key::Esc);
    match a.command("models deepseek") {
        Some(Action::Slash { line }) => assert_eq!(line, "models deepseek"),
        other => panic!("/models NAME must go to the daemon, got {other:?}"),
    }
    // While `/mode` still opens the picker, and `/mode NAME` still types it.
    assert!(matches!(a.command("mode"), Some(Action::Settings) | None));
    assert!(a.pick == Some(Pick::Mode) || a.settings.is_empty());
}
