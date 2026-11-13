//! Quick asks: `alt-k` in the pane. rano owns the prefix, the presets and the target rule, so what
//! is asserted here is the head's half - the prompt that leaves, the range that stays tinted, and
//! the operator's decision riding the prompt after it.

use super::*;
use rano::term::{Event, KeyCode, KeyEvent, Mods};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A file of three paragraphs, so the ask's target rule - the paragraph around the cursor, for
/// prose - has an edge to find rather than the whole file.
const ARTICLE: &str = "article.txt";

const FIRST: &str =
    "The first transatlantic cable was laid in 1866.\nIt carried one message a minute.";
const REWRITTEN: &str =
    "The first transatlantic cable, laid in 1866, carried eight words a minute.";

/// The `.txt` on purpose: rano starts a language server for the languages it knows, and a test has
/// no business starting one.
struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One directory per call, not per tag: two tests asking for the same tag would otherwise remove
/// each other's file mid-run.
fn workspace(tag: &str) -> Workspace {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = letibot_sessionlog::testing::scratch_root()
        .join(format!("letibot-ask-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(ARTICLE),
        format!("{FIRST}\n\nA gap in the record remains.\n\nNobody knows who cut it.\n"),
    )
    .unwrap();
    Workspace(dir)
}

/// A head in that workspace, with the file open and the cursor on its first line.
fn asking(tag: &str) -> (Workspace, App) {
    let ws = workspace(tag);
    let hub = Hub::new("s");
    hub.publish(testing::appended("s.1", "user"));
    hub.record_item(
        "s.1",
        TranscriptItem::User {
            parts: vec![UserPart::Text {
                text: "fact check this".into(),
            }],
            speaker: letibot_transcript::Speaker::Operator,
        },
    );
    let mut a = app();
    a.apply(hello("s", vec![brief("s", "one", false)], hub.snapshot()));
    a.wiring.workspace = ws.0.display().to_string();
    a.open_in_editor(&FileRef::file(ARTICLE));
    settle(&mut a);
    (ws, a)
}

/// rano's ticks until the buffer it opened has arrived: loading is rano's own thread.
fn settle(a: &mut App) {
    for _ in 0..10_000 {
        a.editor_tick(Instant::now());
        if !a.edit_pane.as_ref().is_some_and(|p| p.ed.loading()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    a.editor_tick(Instant::now());
}

fn alt(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), Mods::ALT))
}

fn plain(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), Mods::NONE))
}

/// The text the pane's buffer holds now, as rano reads it out.
fn buffer_text(a: &App) -> String {
    let ed = &a.edit_pane.as_ref().expect("the pane is open").ed;
    ed.buffers[ed.cur].buf.text()
}

/// A read through the router, as the loop delivers it: rano's share is rano's, and what it hands
/// back is the head's, acted on in the same order.
fn press(a: &mut App, events: Vec<Event>) {
    let (keys, _) = a.route(events);
    for k in keys {
        a.key(k);
    }
}

/// The preset rano ships for `alt-k r`, so the words under test are the ones a person gets.
fn preset(id: &str) -> rano::ask::AskPreset {
    rano::ask::default_presets()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("rano ships no {id} preset"))
}

/// What the pane's own mark list says about the asked range: rano holds marks on the buffer, so
/// this is the drawing's own input and not the head's bookkeeping.
fn tinted(a: &App) -> Option<(usize, usize, String)> {
    let p = a.edit_pane.as_ref()?;
    p.ed.marks()
        .iter()
        .find(|m| m.source.starts_with("ask:"))
        .map(|m| (m.line, m.end_line, m.source.clone()))
}

/// **`alt-k r` asks the session.** The preset's own words and the paragraph the cursor is in leave
/// as a prompt at once - with `ask.answer = suggest`, so the daemon teaches the `suggest` fence -
/// and nothing goes through the composer.
#[test]
fn alt_k_r_sends_the_preset_with_the_asked_range() {
    let (_ws, mut a) = asking("preset");
    assert!(a.editor_focused(), "the pane has the keys");
    let rewrite = preset("rewrite");

    press(&mut a, vec![alt('k')]);
    assert!(
        a.editor_focused(),
        "the prefix keeps the keyboard in the pane"
    );
    press(&mut a, vec![plain('r')]);

    let sent = a.take_actions();
    let prompt = sent
        .iter()
        .find_map(|x| match x {
            Action::Prompt(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no prompt left the head: {sent:?}"));
    assert_eq!(prompt.text, rewrite.prompt, "the preset's own words");
    let ctx = prompt.context.as_ref().expect("the ask carries a context");
    let ask = ctx.ask.as_ref().expect("and says it is an ask");
    assert_eq!(ask.preset, "rewrite");
    assert_eq!(
        ask.answer,
        letibot_sessionlog::protocol::AnswerMode::Suggest
    );
    let r = ctx.refs.first().expect("the place rides the ask");
    assert!(r.file.ends_with(ARTICLE), "the file asked about: {r:?}");
    let range = r.range.expect("a range, not a bare place");
    assert_eq!((range.start.line, range.start.column), (1, 1));
    assert_eq!(range.end.line, 2, "the paragraph's last line: {r:?}");
    assert_eq!(
        r.text, FIRST,
        "the selection is the paragraph's own text, not a paste of it"
    );
    assert_eq!(a.input(), "", "no composer round trip");
}

/// The asked range is tinted while the answer is on its way, and the turn that answered it takes the
/// tint down: a highlight the operator cannot get rid of is the defect this is the other half of.
#[test]
fn the_asked_range_is_tinted_until_the_turn_ends() {
    let (_ws, mut a) = asking("tint");
    press(&mut a, vec![alt('k')]);
    press(&mut a, vec![plain('r')]);

    let (first, last, source) =
        tinted(&a).unwrap_or_else(|| panic!("the asked range is not tinted"));
    assert_eq!((first, last), (1, 2), "the tint covers what was asked");
    assert!(
        source.contains(ARTICLE),
        "and it is filed under the file: {source}"
    );

    a.apply(ServerFrame::Event(env(1, testing::turn_started("t1"))));
    a.apply(ServerFrame::Event(env(
        2,
        testing::turn_finished_round("t1"),
    )));
    assert!(
        tinted(&a).is_some(),
        "a round boundary is not the end of the prompt"
    );

    a.apply(ServerFrame::Event(env(3, testing::turn_finished("t1"))));
    assert!(
        tinted(&a).is_none(),
        "the end of the turn takes the tint down"
    );
    assert!(a.file_marks.groups(&a.resolve_path(ARTICLE)).is_empty());
}

/// **Accepting the returned hunk puts one outcome on the next prompt.** The daemon's `suggest`
/// fence becomes a pending hunk, `alt-d` then `a` takes it, and the composer's next message carries
/// what the operator did - which is the only door the session learns it by.
#[test]
fn accepting_a_returned_hunk_puts_one_outcome_on_the_next_prompt() {
    let (_ws, mut a) = asking("decide");
    press(&mut a, vec![alt('k')]);
    press(&mut a, vec![plain('r')]);
    let _ = a.take_actions();

    // The daemon's answer: one `suggest` fence over the paragraph that was asked about, its anchor
    // the lines the file holds.
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::Citations {
            item_id: "s.1".into(),
            citations: vec![letibot_sessionlog::event::Citation {
                label: format!("{ARTICLE}:1-2"),
                path: ARTICLE.into(),
                start: 1,
                end: 2,
                lines: FIRST.lines().map(str::to_string).collect(),
                error: None,
                kind: letibot_sessionlog::CitationKind::Suggest,
                body: REWRITTEN.into(),
            }],
        },
    )));
    let hunks = a
        .edit_pane
        .as_ref()
        .expect("the pane is open")
        .ed
        .marks()
        .to_vec();
    assert!(
        hunks
            .iter()
            .any(|m| m.kind == rano::marks::MarkKind::Suggest),
        "the reply's fence is a hunk in the pane: {hunks:?}"
    );

    // `alt-d` puts the cursor on the hunk and draws it; `a` is the review's accept.
    press(&mut a, vec![alt('d')]);
    press(&mut a, vec![plain('a')]);
    assert_eq!(
        a.outcomes,
        vec![letibot_sessionlog::MarkOutcome {
            label: format!("{ARTICLE}:1-2"),
            outcome: "accepted".into(),
        }],
        "one decision, waiting for the next prompt"
    );
    let text = buffer_text(&a);
    assert!(text.contains(REWRITTEN), "the hunk landed in the buffer");
    assert!(
        !a.edit_pane
            .as_ref()
            .expect("the pane")
            .ed
            .marks()
            .iter()
            .any(|m| m.kind == rano::marks::MarkKind::Suggest),
        "a decided hunk is not offered again"
    );

    // Esc leaves the pane, and the composer's own line is the prompt the decision rides.
    press(&mut a, vec![Event::Key(KeyEvent::plain(KeyCode::Esc))]);
    assert!(!a.editor_focused(), "esc gave the composer the keys");
    for c in "thanks".chars() {
        a.key(Key::Char(c));
    }
    let Some(Action::Prompt(p)) = a.key(Key::Enter) else {
        panic!("the composer's line did not leave as a prompt");
    };
    let ctx = p.context.expect("the decision rides a context");
    assert_eq!(ctx.outcomes, a_outcomes());
    assert!(ctx.ask.is_none(), "a typed line is not an ask");
    assert!(
        a.outcomes.is_empty(),
        "the session has been told, so it is not told again on the prompt after"
    );
}

/// The one outcome the prompt above must carry, spelled out where it is compared.
fn a_outcomes() -> Vec<letibot_sessionlog::MarkOutcome> {
    vec![letibot_sessionlog::MarkOutcome {
        label: format!("{ARTICLE}:1-2"),
        outcome: "accepted".into(),
    }]
}
