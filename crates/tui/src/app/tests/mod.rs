//! **The app's tests**, one file per theme, and the fixtures they share. Each theme file
//! reaches the fixtures, the app and the imports below through `use super::*`.

use super::*;

use crate::ui::render::{RenderConfig, sgr, visible_width, wrap};

use letibot_sessionlog::client::Unreadable;

use letibot_sessionlog::event::{DeltaTarget, SessionEvent, Usage};

use letibot_sessionlog::hub::Hub;

use letibot_sessionlog::protocol::Caps;

use letibot_sessionlog::protocol::ServerFrame;

use letibot_sessionlog::registry::{SessionBrief, SessionWiring};

use letibot_sessionlog::testing;

use letibot_sessionlog::view::{
    CallState, OpenDecision, SettledDecision, Snapshot, SnapshotItem, Warned,
};

use letibot_transcript::{TranscriptItem, UserPart};

use rano::style::Role;

use letibot_ui::text::without_control_lines;

// The compact numbers the head prints — rano's, where letibot's `progress` went.
use rano::agent::card;
use rano::agent::header as progress;
use rano::width::text as width;

fn app() -> App {
    App::new(RenderConfig {
        width: 80,
        color: false,
        ..RenderConfig::default()
    })
}

fn decision_with(kinds: &[letibot_sessionlog::event::OptionKind]) -> OpenDecision {
    use letibot_sessionlog::event::DecisionOption;
    OpenDecision {
        req_id: "d1".into(),
        kind: "permission".into(),
        call_id: None,
        access: String::new(),
        summary: "edit a file".into(),
        target: String::new(),
        write_targets: Vec::new(),
        detail: String::new(),
        options: kinds
            .iter()
            .map(|k| DecisionOption {
                option_id: match k {
                    letibot_sessionlog::event::OptionKind::AllowOnce => "allow_once",
                    letibot_sessionlog::event::OptionKind::AllowSession => "allow_session",
                    letibot_sessionlog::event::OptionKind::AllowAlways => "allow_always",
                    letibot_sessionlog::event::OptionKind::RejectOnce => "deny",
                    letibot_sessionlog::event::OptionKind::RejectAlways => "deny_always",
                }
                .to_string(),
                label: "x".into(),
                kind: *k,
            })
            .collect(),
        choices: vec![],
        because: String::new(),
        advice: None,
        // The fixture is this session's own call — see `SubagentAsk` for the card a
        // child's gate posts here instead.
        subagent: None,
        deadline: None,
        on_timeout: letibot_sessionlog::event::OnTimeout::Deny,
        asked_ts: 0,
    }
}

fn path_fixture() -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "lb-paths-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("stroppy/build")).unwrap();
    std::fs::write(d.join("stroppy/build/stroppy"), "").unwrap();
    std::fs::write(d.join("README.md"), "").unwrap();
    std::fs::write(d.join("README.old"), "").unwrap();
    std::fs::write(d.join(".hidden"), "").unwrap();
    std::fs::write(d.join("my file.txt"), "").unwrap();
    d
}

/// Type into the composer the way a person does, one key at a time. There is
/// no `input` field to assign any more, and that is the point: the composer
/// is a state machine with undo batching and a paste ledger, and a test that
/// reaches past it is not testing what runs.
fn typed(a: &mut App, text: &str) {
    for c in text.chars() {
        a.key(Key::Char(c));
    }
}

// ─────────────────────────── `!term`, the pane ───────────────────────────

/// Open a pane the way the operator does, and feed it the bytes a program would draw.
///
/// The submit is a real key path — typed characters and an Enter — rather than a
/// constructed `Action`, because the recognition at the composer is half of what these
/// tests are about.
fn pane(a: &mut App, bytes: &[u8]) -> Action {
    typed(a, "!term mc");
    let opened = a.key(Key::Enter).expect("the verb opens a pane");
    a.apply(ServerFrame::TermOutput {
        bytes: bytes.to_vec(),
    });
    opened
}

/// Drive an app from a hub the way the real driver does.
fn feed(app: &mut App, hub: &Hub, head_id: &str) -> (u64, u64) {
    let (mut r, mut f) = (0, 0);
    while let letibot_sessionlog::hub::Delivery::Events(b) = hub.next_batch(head_id, 512) {
        for env in b.events() {
            match app.apply(ServerFrame::Event(env.clone())) {
                Disposition::Rendered => r += 1,
                Disposition::Filtered => f += 1,
                Disposition::Control => {}
            }
        }
        if b.len() < 512 {
            break;
        }
    }
    (r, f)
}

/// A parent with two subagents, plus a second conversation with a child of its own —
/// the list the daemon sends on every `Hello`, which is the durable half of the tree.
fn a_family() -> Vec<SessionBrief> {
    let mut one = brief("s-sub-1", "find the bug in the reader", true);
    one.parent_session_id = Some("s".into());
    let mut two = brief("s-sub-2", "audit the store", false);
    two.parent_session_id = Some("s".into());
    let mut far = brief("s-other-sub", "someone else's child", true);
    far.parent_session_id = Some("s-other".into());
    vec![
        brief("s", "parent", false),
        one,
        two,
        brief("s-other", "other conversation", false),
        far,
    ]
}

/// **The head in the child's session, with the daemon's list in hand** — the state Enter
/// leaves it in when the `Switch` is answered.
fn inside_a_subagent() -> App {
    let mut a = app();
    a.apply(hello("s", a_family(), Hub::new("s").snapshot()));
    a.apply(hello("s-sub-1", a_family(), Hub::new("s-sub-1").snapshot()));
    a
}

/// The parse, as the pane would draw it with no colour — so a test asserts
/// the rows and not the escape codes.
fn todo_plain(body: &str) -> String {
    render_todo_md(body)
        .into_iter()
        .flat_map(|r| {
            let pad = " ".repeat(r.indent);
            let head = match r.mark {
                Some(m) => format!("{pad}{} {}", m.glyph(), r.text),
                None => format!("{pad}{}", r.text),
            };
            // The body too, so a test can assert what an unfolded item shows.
            std::iter::once(head).chain(r.body.into_iter().map(move |l| format!("{pad}    {l}")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn finished_costing(turn: &str, micros: Option<u64>) -> SessionEvent {
    SessionEvent::TurnFinished {
        turn_id: turn.into(),
        finish_reason: letibot_sessionlog::event::FinishReason::Eos,
        usage: Usage {
            prompt_tokens: 100,
            cached_tokens: 0,
            predicted_tokens: 10,
            cost_micros_usd: micros,
        },
        timings: letibot_sessionlog::event::Timings {
            prompt_ms: 1.0,
            predicted_ms: 1.0,
            wall_ms: 2,
        },
    }
}

/// Feed the two rows a `!` line produces, the way the daemon's append reaches a
/// head: the operator's own `User` row, then the `bash` result with its `origin`
/// set. No proposing assistant row exists — that is the point — so nothing else is
/// applied, which is what makes this the shape a switched-in head receives too.
fn bang_rows(a: &mut App, seq: u64, id: &str, line: &str, payload: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended(&format!("{id}u"), "user"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::TranscriptContent {
            item_id: format!("{id}u"),
            item: Box::new(TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![letibot_transcript::UserPart::Text {
                    text: line.to_string(),
                }],
            }),
        },
    )));
    a.apply(ServerFrame::Event(env(
        seq + 2,
        testing::appended(id, "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 3,
        SessionEvent::TranscriptContent {
            item_id: id.into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: format!("{id}c"),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: payload.into(),
                edit: None,
                origin: Some(letibot_transcript::CallOrigin::Operator { who: "dead".into() }),
                media: None,
            }),
        },
    )));
}

/// **The control for the operator-act ruling**: the same row with no `origin`, which is a
/// call the MODEL proposed — and also every row written before `CallOrigin` existed, since
/// a missing key reads as `None`. No `User` row goes with it: nobody at the keyboard acted.
fn model_bash_rows(a: &mut App, seq: u64, id: &str, payload: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::appended(id, "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::TranscriptContent {
            item_id: id.into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: format!("{id}c"),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: payload.into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
}

/// **Every escape sequence this head has any business emitting.**
///
/// The frame's own vocabulary, stated positively: the `sgr` module's constants, which is what
/// `colour()`, `dim()` and the git field paint with, and every sequence `Palette::open` can
/// produce for a role. §3.1's guarantee is *a foreign program's escape never reaches the
/// terminal*; this is the other half of the same sentence — what does reach it is a sequence
/// this head chose, and the list is short enough to write down.
///
/// A helper for the coloured tests, because the assertion it feeds is one line and the list
/// would otherwise be copied into each of them: `strip these off the row and nothing a
/// terminal would act on is left`.
fn head_vocabulary() -> Vec<String> {
    let mut v: Vec<String> = [
        sgr::RESET,
        sgr::BOLD,
        sgr::DIM,
        sgr::ITALIC,
        sgr::BOLD_ITALIC,
        sgr::CYAN,
        sgr::GREEN,
        sgr::YELLOW,
        sgr::RED,
        sgr::MAGENTA,
        sgr::GREY,
        sgr::REVERSE,
    ]
    .map(String::from)
    .to_vec();
    for r in [
        Role::Plain,
        Role::Faint,
        Role::Strong,
        Role::Heading,
        Role::Subheading,
        Role::UserAccent,
        Role::UserBlock,
        Role::Success,
        Role::Pending,
        Role::Failure,
        Role::Attention,
        Role::Reasoning,
        Role::Code,
        Role::Added,
        Role::Removed,
        Role::Emphasis,
        Role::Keyword,
        Role::StringLit,
        Role::NumberLit,
        Role::Comment,
        Role::TypeName,
        Role::FuncName,
    ] {
        let o = rano::style::Palette::Colour.open(r);
        if !o.is_empty() {
            v.push(o);
        }
    }
    v
}

/// **Take this head's own vocabulary off a row; nothing a terminal would act on may be left.**
///
/// A `String::replace` per sequence rather than an escape parser in the test: the question is
/// membership, the vocabulary is a closed list, and a second parser here would be a second
/// thing to keep in step with the first — which is the mistake this whole change is about.
fn only_the_heads_own_escapes(row: &str) -> bool {
    let mut rest = row.to_string();
    for own in head_vocabulary() {
        rest = rest.replace(&own, "");
    }
    !rest.contains('\u{1b}')
        && !rest
            .chars()
            .any(|c| ('\u{80}'..='\u{9f}').contains(&c) || c == '\u{7f}')
}

/// **The operator's own `ls -la`, byte for byte, as they pasted it.**
///
/// Not a fixture invented for a test: this is the payload from the report — the header line
/// `total 124` and three directory rows — with every `ESC` the program wrote in it. The
/// synthetic payloads the first cut used (`"src/\u{1b}[01;34mthe-dir\u{1b}[0m…"`) had two
/// things the real one has and they did not: a **line with no escape on it at all**, and
/// `ls`'s own `ESC[0m` **before** the colour rather than only after it. The paste's `ESC`
/// bytes are invisible in a transcript, which is why the pasted text reads `[0m[01;34m.` —
/// a `[`-sequence body with no introducer in front of it.
const OPERATOR_LS_LA: &str = "total 124\n\
        drwxrwxr-x 22 dead dead  4096 Oct  6 09:46 \u{1b}[0m\u{1b}[01;34m.\u{1b}[0m\n\
        drwxrwxr-x  3 dead dead  4096 Oct  4 22:22 \u{1b}[01;34mcrates\u{1b}[0m\n\
        drwxrwxr-x 14 dead dead  4096 Oct  6 11:11 \u{1b}[01;34mletibot\u{1b}[0m\n";

/// A `bash` result row, with the payload as the store holds it.
fn bash_result(payload: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "bang-1".into(),
        name: "bash".into(),
        outcome: letibot_transcript::ToolOutcome::Ok,
        payload: payload.into(),
        edit: None,
        origin: Some(letibot_transcript::CallOrigin::Operator { who: "dead".into() }),
        media: None,
    }
}

/// **What a reader actually sees on a row** — every escape this head wrote taken off it.
///
/// `without_control` is the transcript crate's own sequence parser, so this is the frame's
/// bytes with the head's vocabulary removed rather than a second parser written here. A
/// `[` left in the result is a sequence's **body** that reached the glass as text: the
/// defect the operator pasted, `[0m[01;34m`, is exactly that — an introducer that is gone
/// and five characters that are not.
fn seen(row: &str) -> String {
    letibot_transcript::sanitize::without_control(row)
}

/// The short spellings `command()` keeps taking and the table deliberately does not
/// carry: offering both spellings doubles the list to teach the same actions.
const HEAD_COMMAND_ALIASES: &[&str] = &["?", "h", "q", "r", "s", "t", "v", "i"];

/// **§3.1: content this head did not write must not reconfigure the terminal.**
///
/// The exposure is real and it is this head's specifically, not a shared habit:
/// leticl composes styled segments into a **cell grid** and `screen-put-string`
/// writes only cluster text, so it *cannot* emit an escape it did not author. This
/// head composes ANSI-bearing strings and `paint_full` writes each row verbatim, so
/// an escape in model prose went straight to whatever terminal was attached.
///
/// The sequence used is the one an operator can **watch happen**: `ESC ] 0 ; x BEL`
/// is the window-title request, so a rendered one renames the window they are
/// working in. It contains `ESC` and `BEL`, which after `without_control_lines`
/// become two spaces — so the *literal* sequence cannot appear in any row, which is
/// what this asserts rather than "the row looks fine".
///
/// Every surface §3.1 names is walked: model prose (settled and live), reasoning,
/// the operator's own paste, the system row, a tool payload and its failure reason,
/// the raw markup behind `ctrl-x`, and both sides of a diff read off disk.
const EVIL: &str = "\u{1b}]0;pwned\u{7}";

const EVIL_HEAD: &str = "\u{1b}]0;";

fn render_one(item: letibot_transcript::TranscriptItem) -> String {
    item_rows(false, item).join("\n")
}

/// The rows one transcript item renders to, at a chosen colour setting.
///
/// Colour is a parameter because the falsification test below needs both: with no
/// colour the head emits **no escapes at all**, which makes "not one ESC" a precise
/// assertion over the text; with colour it emits its own, so the assertion narrows
/// to the sequences it never writes.
fn item_rows(colour: bool, item: letibot_transcript::TranscriptItem) -> Vec<String> {
    use letibot_transcript::ToolEditExcerpt;
    let cfg = RenderConfig {
        width: 120,
        color: colour,
        ..RenderConfig::default()
    };
    let targets = std::collections::HashMap::new();
    let answered = std::collections::HashSet::new();
    // A file whose bytes carry the sequence, because the diff is the one surface
    // where the content is neither the model's nor the daemon's — it is whatever
    // is on disk.
    let excerpt = ToolEditExcerpt {
        path: format!("src/{EVIL}thing.rs"),
        created: false,
        before_start: 1,
        after_start: 1,
        before_lines: 2,
        after_lines: 2,
        truncated: false,
        before: format!("let a = 1;{EVIL}\nlet b = 2;\n"),
        after: format!("let a = 3;{EVIL}\nlet b = 2;\n"),
    };
    let edit = item_kind_is_edit(&item).then_some(&excerpt);
    let it = SnapshotItem {
        item_id: "s.1".into(),
        kind: "row".into(),
        ledger_head: String::new(),
        ts: 0,
        item: Some(item),
    };
    let ctx = ItemCtx {
        cfg: &cfg,
        think: Fold::Open,
        tools: Fold::Open,
        raw: true,
        targets: &targets,
        answered: &answered,
        subagents: &[],
        drawn_live: false,
        elapsed_ms: None,
        edit,
        decision: None,
        bound: None,
        echo_mark: QUEUED,
        echo_open: false,
        vis: Visibility::lifted(),
        diff_split: true,
        payload_view: None,
        payload_max: None,
        window_rows: usize::MAX,
        payload_newest: None,
    };
    item_lines(&it, &ctx).1
}

fn item_kind_is_edit(i: &letibot_transcript::TranscriptItem) -> bool {
    use letibot_transcript::TranscriptItem as T;
    matches!(i, T::ToolResult { name, .. } if matches!(name.as_str(), "edit" | "write"))
}

/// **The store's own vocabulary of hostile bytes.**
///
/// Every family below was found in the operator's store — 44 `tool_result` rows
/// carry an escape and 20 carry a mode string — so this is the corpus's vocabulary
/// and not a list invented for a test. SGR and reset are in it because they are
/// what a coloured command emits; the rest are the things a terminal *does* rather
/// than shows.
const HOSTILE: &str = " A\u{1b}[31mred\u{1b}[0m \u{1b}[8m(hidden) \u{1b}[2J \
        \u{1b}[?1002h \u{1b}[?1006h \u{1b}[?1049h \u{1b}[?2004h \u{1b}[?2026h \
        \u{1b}]0;pwned\u{7} \u{9b}31m \u{9c} \u{7f} end";

/// **Sequences a frame body never contains**, whatever the palette is doing.
///
/// The ambiguous ones are deliberately absent: an SGR pair in model prose is
/// **indistinguishable in the byte stream from this head's own red** — the palette
/// emits `ESC[31m` itself — so asserting its absence there would be either false or
/// meaningless. It is caught by the colourless check instead, where the head emits
/// no escapes at all and "not one ESC" is a precise statement about the text.
///
/// What is left is unambiguous: finding one of these is proof that somebody else's
/// bytes reached the terminal.
const NEVER_IN_A_FRAME: &[(&str, &str)] = &[
    ("an OSC window title", "\u{1b}]"),
    ("mouse tracking", "\u{1b}[?1002"),
    ("SGR mouse", "\u{1b}[?1006"),
    ("the alternate screen", "\u{1b}[?1049"),
    ("bracketed paste", "\u{1b}[?2004"),
    ("synchronised update", "\u{1b}[?2026"),
    ("conceal", "\u{1b}[8m"),
    ("a screen clear", "\u{1b}[2J"),
    ("a C1 CSI", "\u{9b}"),
    ("a C1 ST", "\u{9c}"),
    ("a DEL", "\u{7f}"),
];

/// The first thing in these rows a terminal would act on and this head did not
/// write — or `None` when the rows are clean.
fn unauthored(rows: &[String]) -> Option<String> {
    for r in rows {
        for (what, seq) in NEVER_IN_A_FRAME {
            if r.contains(seq) {
                return Some(format!("{what}: {seq:?} in {r:?}"));
            }
        }
        if let Some(c) = r
            .chars()
            .find(|c| *c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(c))
        {
            return Some(format!("a C1/DEL byte {c:?} in {r:?}"));
        }
    }
    None
}

/// A tool result row whose body has landed, the way the daemon delivers one: an
/// announcement, then its content. A local helper rather than a `testing::` one
/// because the fixture's own `result` is inside another test function.
/// **How many markers a screen carries** — the number, which is the assertion R37
/// AMENDED needs.
///
/// **Counted by the COUNTS, now that the seam is gone** — the operator: *"dont print \" dot
/// /verbosity\" or ctrl-t opens it - we dont need that."*
///
/// This used to count the seam, which is what a marker had and no other row did — a reliable
/// signature while it existed. What a marker has now is a bracket holding a digit and one of
/// the count nouns, at any rung of the ladder (`2 tool calls` down to `2t`), so that is the
/// test. Counting `[` alone would count a payload's own text, which is what the seam was
/// standing in for.
fn markers(screen: &str) -> usize {
    screen.lines().filter(|l| is_marker(l)).count()
}

/// One line, as a marker: `[…]` holding a digit and a count noun.
fn is_marker(line: &str) -> bool {
    let Some(open) = line.find('[') else {
        return false;
    };
    let Some(close) = line[open + 1..].find(']') else {
        return false;
    };
    let inner = &line[open + 1..open + 1 + close];
    inner.starts_with(|c: char| c.is_ascii_digit())
        && ["tool", "call", "thinking", "line", "calls", "lines"]
            .iter()
            .any(|w| inner.contains(w))
}

fn a_result_row(a: &mut App, seq: u64, id: &str, payload: &str) {
    a.apply(ServerFrame::Event(env(
        seq,
        testing::proposed_on("t1", "c1", "bash", "cargo test"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        testing::appended(id, "tool_result"),
    )));
    a.apply(ServerFrame::Event(env(
        seq + 2,
        SessionEvent::TranscriptContent {
            item_id: id.into(),
            item: Box::new(TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: payload.into(),
                edit: None,
                origin: None,
                media: None,
            }),
        },
    )));
}

/// An assistant row carrying one `bash` call, the way the session's log carries it.
fn bash_row(cmd: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: String::new(),
        tool_calls: vec![letibot_transcript::ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: format!(r#"{{"command": {cmd:?}}}"#),
        }],
        truncated: false,
    }
}

/// **A `Hello` carrying a snapshot of `n` body-less rows** — the R9 tests' fixture.
///
/// A snapshot full of rows with no bodies is a *bulk announcement*, and building one
/// means going through a real `Hub` so the snapshot is the daemon's own shape rather
/// than a hand-rolled one.
fn snapshot_hello(tag: &str, n: usize) -> ServerFrame {
    let hub = Hub::new(tag);
    for i in 0..n {
        hub.publish(SessionEvent::TranscriptAppended {
            item_id: format!("{tag}.{i}"),
            kind: "user".into(),
            ledger_head: String::new(),
        });
    }
    let att = hub.attach("tui", "test", Caps::default(), 0);
    assert_eq!(
        att.snapshot
            .as_ref()
            .expect("a snapshot")
            .items
            .iter()
            .filter(|i| i.item.is_none())
            .count(),
        n,
        "the premise: the snapshot carries {n} rows with no body"
    );
    ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: tag.into(),
        head_id: att.head_id.clone(),
        dropped: att.dropped,
        snapshot: Some(Box::new(att.snapshot.expect("a snapshot"))),
        resumed_from: att.resumed_from,
        scrubbed: att.scrubbed,
        wiring: Default::default(),
        sessions: Vec::new(),
    }
}

/// A fixture with the two things the operator's screen had in it: a heading
/// and an inline code span, inside the model's reasoning.
const REASONING_WITH_MARKDOWN: &str = "## The plan\n\nFirst read `crates/tui/src/render.rs`, then look at the `Decor` type, because **that** is where the style has to be restored, and the rest of this sentence has to stay the reasoning colour even though it wraps onto another row.\n\n### Then\n\nOrdinary prose, still grey.\n\n";

fn brief(id: &str, title: &str, running: bool) -> SessionBrief {
    SessionBrief {
        session_id: id.into(),
        title: title.into(),
        created_ms: 0,
        live: true,
        stored_items: 0,
        status: letibot_sessionlog::SessionStatus {
            session_id: id.into(),
            seq: 4,
            items: 6,
            heads: 1,
            running,
            model: "qwen-3.8-flash-next".into(),
            last_ms: 0,
        },
        wiring: wiring(),
        parent_session_id: None,
        context_tokens: None,
        context_cached: None,
        stored_end: None,
    }
}

fn wiring() -> SessionWiring {
    SessionWiring {
        model: "qwen-3.8-flash-next".into(),
        dialect: "qwen3.8".into(),
        endpoint: "127.0.0.1:8080".into(),
        workspace: "/home/dead/Projects/letibot".into(),
    }
}

/// **The daemon's door row**, as `harnessd` publishes it — R31, R32, R34.
///
/// A `ServerFrame::Settings` carrying one `head-run.tools` row, because that is the only
/// way a head learns any of this: it holds no schema, and these tests would prove nothing
/// if they handed the app a struct it built itself from knowledge the head does not have.
/// Empty `field` and `why_json` is the no-bare-form case.
fn door_row(name: &str, field: &str, kind: &str) -> ServerFrame {
    ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: letibot_sessionlog::HEAD_RUN_TOOLS_KEY.into(),
            value: name.into(),
            source: "default".into(),
            editable: String::new(),
            choices: Vec::new(),
            tools: vec![letibot_sessionlog::HeadRunTool {
                name: name.into(),
                field: field.into(),
                kind: kind.into(),
                defaults: Default::default(),
                why_json: if field.is_empty() {
                    format!("`{name}` takes its arguments as JSON: it needs 2 fields (a, b)")
                } else {
                    String::new()
                },
            }],
        }],
    }
}

fn hello(session: &str, sessions: Vec<SessionBrief>, snapshot: Snapshot) -> ServerFrame {
    hello_at(
        session,
        sessions,
        snapshot,
        letibot_sessionlog::protocol::PROTOCOL_VERSION,
    )
}

/// The same, with the version the daemon claims. A separate helper rather than a
/// parameter on `hello` because every other test wants the honest value, and a default
/// argument that could be forgotten is how a test ends up asserting against a skew it
/// did not mean to create.
fn hello_at(
    session: &str,
    sessions: Vec<SessionBrief>,
    snapshot: Snapshot,
    protocol_version: u32,
) -> ServerFrame {
    ServerFrame::Hello {
        protocol_version,
        session_id: session.into(),
        head_id: "h1".into(),
        dropped: 0,
        snapshot: Some(Box::new(snapshot)),
        resumed_from: None,
        scrubbed: Default::default(),
        wiring: wiring(),
        sessions,
    }
}

/// **R20 — the measurement first, and then the fix.** A card whose content is taller
/// than the screen, on the glass.
///
/// The operator, on a permission card carrying a giant replace or a commit message:
/// *"I'm shown a permission prompt and I just cant see the selector."* The mechanism
/// was the screen-fit loop's `dec_rows -= 1`, which trims **from the end** — and the
/// end of a card is the ladder, the deadline and the hint. **Measured before the fix,
/// 80x24 with a 42-row card: the headline, the target and nineteen lines of layer A's
/// reading, and the three options GONE.** The composer was still there, so the
/// operator held a screen with a question on it and no way to answer it.
///
/// After: the ladder in full, then the hint, and a seam saying how many lines are out
/// of view and which key reads them. The content still starts at its head, because
/// that is where a card is read from.
fn tall_card(tall: bool) -> App {
    use letibot_sessionlog::event::OptionKind;
    let mut a = app();
    let mut d = decision_with(&[
        OptionKind::AllowOnce,
        OptionKind::AllowSession,
        OptionKind::RejectOnce,
    ]);
    d.summary = "edit a file".into();
    d.target = "crates/tui/src/app.rs".into();
    d.detail = if tall {
        (0..40)
            .map(|i| format!("  line {i} of layer A's reading of this call"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "the reading".into()
    };
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::DecisionRequested {
            write_targets: Vec::new(),
            req_id: d.req_id.clone(),
            kind: d.kind.clone(),
            call_id: None,
            access: "write".into(),
            summary: d.summary.clone(),
            target: d.target.clone(),
            detail: d.detail.clone(),
            options: d.options.clone(),
            choices: Vec::new(),
            because: String::new(),
            advice: None,
            subagent: None,
            deadline: None,
            on_timeout: d.on_timeout,
        },
    )));
    a
}

/// A `Subagent` event as the daemon publishes one — the state word, and the child's
/// answer's first line **on a completion only**.
///
/// `harness.rs` is the one publisher and it says which is which: `publish("opening", …, None)`,
/// `publish("failed", …, None)`, `publish("running", …, None)`, and
/// `publish("done", &first_line, Some(first_line.clone()))`. So `answer.is_some()` is
/// exactly *the completion arrived*, which is why the row can be asked whether a child has
/// finished without consulting a clock or an instant.
fn child_event(id: &str, state: &str, answer: Option<&str>) -> SessionEvent {
    SessionEvent::Subagent {
        subagent_id: id.into(),
        state: state.into(),
        prompt: "find the bug in the reader".into(),
        role: "coder".into(),
        task: "find the bug in the reader".into(),
        model: String::new(),
        answer: answer.map(str::to_string),
    }
}

/// **The footer's `N subagents running` line, off the glass** — read from the frame rather
/// than recomputed, so the assertion is about what the operator sees.
fn count_row(a: &mut App) -> String {
    let screen = a.screen(100, 24);
    screen
        .iter()
        .find(|l| l.contains("subagent"))
        .cloned()
        .unwrap_or_else(|| panic!("no subagent count on the top edge:\n{}", screen.join("\n")))
}

fn mode_settings(value: &str, choices: &[&str]) -> ServerFrame {
    ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: "mode".into(),
            value: value.into(),
            source: "project store (modes.tsv)".into(),
            editable: "/mode NAME".into(),
            choices: choices.iter().map(|s| (*s).to_string()).collect(),
            tools: Vec::new(),
        }],
    }
}

fn model_settings(value: &str, choices: &[&str]) -> ServerFrame {
    ServerFrame::Settings {
        rows: vec![letibot_sessionlog::protocol::SettingRow {
            key: "model".into(),
            value: value.into(),
            source: String::new(),
            editable: "/models PROVIDER/MODEL".into(),
            choices: choices.iter().map(|s| (*s).to_string()).collect(),
            tools: Vec::new(),
        }],
    }
}

/// **`Mode::NAMED`'s names verbatim, spaces and all.** This said `writes-allowed` for a long
/// time — a hyphen the name has never had — and that is how a defect where the CURRENT MODE's
/// name contains a space passed a suite that looked like it covered the card: the fixture's
/// list agreed with the code's assumption instead of with the daemon's bytes.
const MODES: &[&str] = &[
    "read-only",
    "always-ask",
    "writes allowed",
    "automode",
    "automode-edits",
    "allow-all",
];

fn the_counts_are_plain_and_the_seam_is_faint_body() {
    let mut a = app();
    a.cfg.color = true;
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        SessionEvent::TranscriptAppended {
            item_id: "s.0".into(),
            kind: "assistant".into(),
            ledger_head: String::new(),
        },
    )));
    a.apply(ServerFrame::Event(env(
        2,
        SessionEvent::TranscriptContent {
            item_id: "s.0".into(),
            item: Box::new(TranscriptItem::Assistant {
                text: "first the helpers:".into(),
                tool_calls: Vec::new(),
                truncated: false,
            }),
        },
    )));
    a_result_row(&mut a, 3, "s.1", "one");
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.invalidate_history();
    let screen = a.screen(120, 30).join("\n");
    // **Glued to the model's sentence**: the counts in the sentence's own register, and
    // only the seam in the faint one.
    assert!(
        screen.contains("first the helpers: [1 tool call]"),
        "the counts are not plain, or are not glued to the sentence: {screen:?}"
    );
    // And the negation, so a future decision to dim the counts fails here rather than
    // passing on a substring: the faint code must not open the marker.
    assert!(
        !screen.contains("\x1b[2m[1 tool call]"),
        "the counts were painted faint: {screen:?}"
    );

    // **Alone, after the operator's message** — the other placement, same registers.
    let mut b = app();
    b.cfg.color = true;
    b.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    for (seq, id, kind, item) in [
        (
            1u64,
            "s.0",
            "user",
            TranscriptItem::User {
                speaker: letibot_transcript::Speaker::Operator,
                parts: vec![letibot_transcript::UserPart::Text {
                    text: "run the tests".into(),
                }],
            },
        ),
        (
            3,
            "s.1",
            "tool_result",
            TranscriptItem::ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                outcome: letibot_transcript::ToolOutcome::Ok,
                payload: "output".into(),
                edit: None,
                origin: None,
                media: None,
            },
        ),
    ] {
        b.apply(ServerFrame::Event(env(
            seq,
            SessionEvent::TranscriptAppended {
                item_id: id.into(),
                kind: kind.into(),
                ledger_head: String::new(),
            },
        )));
        b.apply(ServerFrame::Event(env(
            seq + 1,
            SessionEvent::TranscriptContent {
                item_id: id.into(),
                item: Box::new(item),
            },
        )));
    }
    b.visibility = Visibility::of(Profile::CONVERSATION);
    b.invalidate_history();
    let screen = b.screen(120, 30);
    let line = screen
        .iter()
        .find(|l| l.contains("[1 tool call]"))
        .expect("the counts are on the screen");
    assert!(
        line.contains("[1 tool call]"),
        "the lone marker's registers are wrong: {line:?}"
    );
    // And it is genuinely its own line, not glued to the operator's words.
    let theirs = screen
        .iter()
        .position(|l| l.contains("run the tests"))
        .expect("the message is on the screen");
    assert!(
        !screen[theirs].contains("tool call"),
        "the counts are on the operator's own line: {:?}",
        screen[theirs]
    );
}

/// **A turn mid-run, as the daemon drives one** — the narration that introduces the work, one
/// round's result row, and reasoning streaming with no row of its own yet.
///
/// The shape is the operator's own, and it is the one that makes a second marker legible: the
/// run is the hidden row after the prose, and the only work in flight is thinking — so a
/// marker drawn beside the run reads `[N thinking lines]` next to `[1 tool call]`, which is
/// the pair they reported.
fn a_turn_mid_run(a: &mut App) {
    a.visibility = Visibility::of(Profile::CONVERSATION);
    a.apply(hello(
        "s",
        vec![brief("s", "one", false)],
        Hub::new("s").snapshot(),
    ));
    a.apply(ServerFrame::Event(env(
        1,
        testing::appended("s.0", "assistant"),
    )));
    a.record_item(
        "s.0",
        TranscriptItem::Assistant {
            text: "let me check that for you:".into(),
            tool_calls: Vec::new(),
            truncated: false,
        },
    );
    // ROUND 1, carrying the prompt's own stamp — see `TurnPane::turn_rows`.
    a.apply(ServerFrame::Event(env_at(
        2,
        1_000,
        SessionEvent::TurnStarted {
            turn_id: "r1".into(),
            model: "qwen3-next-80b".into(),
            ledger_head: "0000".into(),
            began_ms: Some(1_000),
        },
    )));
    a.apply(ServerFrame::Event(env(
        3,
        testing::appended("s.1", "tool_result"),
    )));
    a.record_item(
        "s.1",
        TranscriptItem::ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload: "SOMETHING LONG ENOUGH TO HIDE THE ROW".into(),
            edit: None,
            origin: None,
            media: None,
        },
    );
    // The work in flight: thinking, and no row of its own — the pane's half of the counts.
    a.apply(ServerFrame::Event(env(
        4,
        SessionEvent::Delta {
            turn_id: "r1".into(),
            target: DeltaTarget::Reasoning,
            text: "the first call told me it is in the reader, so let me check the caller\n\
                       and the place it is constructed before I touch anything at all"
                .into(),
        },
    )));
}

/// Announce a transcript row and attach its body, the way the daemon's
/// append reaches a head.
fn shell_row(a: &mut App, seq: u64, id: &str, kind: &str, item: TranscriptItem) {
    a.apply(ServerFrame::Event(env(seq, testing::appended(id, kind))));
    a.apply(ServerFrame::Event(env(
        seq + 1,
        SessionEvent::TranscriptContent {
            item_id: id.into(),
            item: Box::new(item),
        },
    )));
}

/// An operator's own row, the way the session's log carries it.
fn operator_row(text: &str) -> TranscriptItem {
    TranscriptItem::User {
        speaker: letibot_transcript::Speaker::Operator,
        parts: vec![UserPart::Text { text: text.into() }],
    }
}

/// The one ask a Tab on an unmatched `!` prefix queues, and the id it minted.
///
/// **Exactly one**: the whole point of the cache is that a Tab asks once, so a
/// helper that took the first and ignored the rest would be blind to the defect it
/// exists to catch.
fn the_one_ask(a: &mut App) -> (String, String) {
    let actions = a.take_actions();
    assert_eq!(actions.len(), 1, "exactly one action: {actions:?}");
    match actions.into_iter().next().expect("one") {
        Action::SuggestShell {
            prefix,
            client_request_id,
        } => (prefix, client_request_id),
        other => panic!("not a SuggestShell: {other:?}"),
    }
}

/// The daemon's answer to an ask, the way it arrives on the pump.
fn model_answers(a: &mut App, id: &str, prefix: &str, lines: &[&str]) {
    a.apply(ServerFrame::ShellSuggestions {
        client_request_id: id.into(),
        prefix: prefix.into(),
        lines: lines.iter().map(|l| l.to_string()).collect(),
    });
}

fn env(seq: u64, event: SessionEvent) -> letibot_sessionlog::event::Envelope {
    env_at(seq, 0, event)
}

/// An envelope with a `ts`. The elapsed times a card shows come from the
/// log's own clock, so a test that wants one has to supply it.
fn env_at(seq: u64, ts: u64, event: SessionEvent) -> letibot_sessionlog::event::Envelope {
    letibot_sessionlog::event::Envelope {
        session_id: "s".into(),
        seq,
        ts,
        event,
    }
}

/// A finished edit call carrying both sides of a small change.
fn edit_row(edit: Option<letibot_sessionlog::event::ToolEdit>) -> CallRow {
    CallRow {
        call_id: "c1".into(),
        name: "edit".into(),
        target: "a.rs".into(),
        state: CallState::Finished {
            outcome: letibot_transcript::ToolOutcome::Ok,
            payload_digest: "fnv1a:1".into(),
            inline_bytes: 64,
            full_bytes: 64,
            spill: None,
            edit,
        },
        started_ms: 1_000,
        started_at: 0,
        ended_ms: 2_000,
        note: None,
        decision: None,
    }
}

/// Flip the diff view the way the operator does now: the config pane's
/// first row. `/diff` is gone — one place to change a setting, not two.
fn flip_diff_view(a: &mut App) {
    let was = a.config_pane;
    a.config_pane = true;
    a.config_sel = 0;
    a.key(Key::Enter);
    a.config_pane = was;
}

fn edit_excerpt() -> letibot_sessionlog::event::ToolEdit {
    letibot_sessionlog::event::ToolEdit {
        path: "a.rs".into(),
        created: false,
        before_start: 1,
        after_start: 1,
        before_lines: 2,
        after_lines: 3,
        truncated: false,
        before: "fn a() {}\n".into(),
        after: "fn a() {\n    x();\n}\n".into(),
    }
}

fn plain_cfg(width: usize) -> RenderConfig {
    RenderConfig {
        width,
        color: false,
        ..Default::default()
    }
}

fn png_header(w: u32, h: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    png.extend_from_slice(&w.to_be_bytes());
    png.extend_from_slice(&h.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    png
}

fn placeholders(screen: &[String]) -> usize {
    screen.iter().filter(|l| l.contains('\u{10EEEE}')).count()
}

/// **The same row, with the name the caller stated.** The slug is the agent's own intent and
/// arrives on the wire beside the command; nothing derives one.
fn named_job(
    id: &str,
    slug: &str,
    command: &str,
    running: bool,
) -> letibot_sessionlog::protocol::JobEntry {
    letibot_sessionlog::protocol::JobEntry {
        slug: slug.into(),
        ..daemon_job(id, command, running)
    }
}

/// The daemon's answer to `ListJobs`, which is the only way a row gets here.
fn daemon_job(id: &str, command: &str, running: bool) -> letibot_sessionlog::protocol::JobEntry {
    letibot_sessionlog::protocol::JobEntry {
        id: id.into(),
        command: command.into(),
        // **Unnamed by default**, which is the row every one of these tests was written
        // against; `named_job` is the one that states a name.
        slug: String::new(),
        how: "asked".into(),
        state: if running {
            "running".into()
        } else {
            "exited 0".into()
        },
        running,
        never_ran: false,
        redirect: None,
        produced: 155,
        elapsed_ms: 14_600,
    }
}

fn jobs_frame(session_id: &str, jobs: Vec<letibot_sessionlog::protocol::JobEntry>) -> ServerFrame {
    ServerFrame::Jobs {
        session_id: session_id.into(),
        jobs,
    }
}

fn backgrounded_finished(handle: &str, call_id: &str) -> SessionEvent {
    SessionEvent::ToolFinished {
        turn_id: "t1".into(),
        call_id: call_id.into(),
        outcome: letibot_transcript::ToolOutcome::Backgrounded {
            handle: handle.into(),
            ran_for_ms: 0,
            how: letibot_transcript::Backgrounding::Asked,
            next: "job_wait".into(),
        },
        payload_digest: "fnv1a:1".into(),
        inline_bytes: 0,
        full_bytes: 0,
        spill: None,
        repairs: 0,
        edit: None,
    }
}

fn proposed_bash(call_id: &str, target: &str) -> SessionEvent {
    SessionEvent::ToolCallProposed {
        turn_id: "t1".into(),
        call_id: call_id.into(),
        name: "bash".into(),
        args_digest: "fnv1a:2".into(),
        target: target.into(),
    }
}

// ---- the starter todos: leticl's `todo_template`, copied ----

/// A head seated in `workspace`, its prefs in `dir`, and a template written to
/// `dir/todo-template.md` — the arrangement the switch names. The workspace rides the
/// Hello itself, because the seed's gate reads it at the attach, where the arm has just
/// set it.
fn seeded_head(dir: &std::path::Path, template: &str, workspace: &str) -> App {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("todo-template.md"), template).unwrap();
    let mut a = app();
    a.prefs_path = Some(dir.join("head.toml"));
    a.load_prefs();
    a.todo_template = crate::prefs::TodoTemplate::Default;
    let snap = Hub::new("s1").snapshot();
    a.apply(ServerFrame::Hello {
        protocol_version: letibot_sessionlog::protocol::PROTOCOL_VERSION,
        session_id: "s1".into(),
        head_id: "h1".into(),
        dropped: 0,
        snapshot: Some(Box::new(snap)),
        resumed_from: None,
        scrubbed: Default::default(),
        wiring: SessionWiring {
            model: String::new(),
            dialect: String::new(),
            endpoint: String::new(),
            workspace: workspace.into(),
        },
        sessions: Vec::new(),
    });
    a
}

// ===== The merge queue pane =====
//
// The pane is the HEAD's half of the queue, and it is drawn from two arrivals: the
// snapshot the daemon answers `ListMergeQueue` with, and the `MergeEntryAdded` /
// `MergeEntryMoved` events that keep it current. These tests are about this head's side
// of that — what it asks for, what it folds, and what it draws — and not about the queue
// itself, which is `harnessd`'s and is tested where it lives.

fn queue_entry(
    id: &str,
    state: letibot_sessionlog::event::MergeState,
) -> letibot_sessionlog::event::MergeEntry {
    letibot_sessionlog::event::MergeEntry {
        id: id.into(),
        session_id: "s1".into(),
        branch: "agent/child-one".into(),
        base_sha: "abc123".into(),
        priority: letibot_sessionlog::event::MergePriority::Subagent,
        needs: Vec::new(),
        state,
        brief: "make the widget blue".into(),
        evidence: String::new(),
        created_ms: 1_000,
        updated_ms: 1_000,
        worktree: None,
        landed_sha: None,
        gate_steps: Vec::new(),
    }
}

fn queue_review(entry_id: &str, decision: Option<&str>) -> letibot_sessionlog::event::MergeReview {
    letibot_sessionlog::event::MergeReview {
        entry_id: entry_id.into(),
        session_id: "reviewer".into(),
        branch: "agent/child-one".into(),
        base_sha: "abc123".into(),
        asked_ms: 2_000,
        answered_ms: decision.map(|_| 3_000),
        decision: decision.map(str::to_string),
        failure: String::new(),
        reasons: vec!["the ask is met and the tests pass".into()],
        files: vec!["crates/widget.rs".into()],
        commands: vec!["cargo test -p widget".into()],
    }
}

/// **A review whose ATTEMPT failed** — asked for, no verdict, and the provider's own words on
/// the row. The operator's four entries were in exactly this shape, and the pane drew them as
/// *the reviewer has been asked and has not answered*: the failure was invisible without opening
/// the store.
fn queue_review_failed(entry_id: &str, failure: &str) -> letibot_sessionlog::event::MergeReview {
    letibot_sessionlog::event::MergeReview {
        entry_id: entry_id.into(),
        session_id: "reviewer".into(),
        branch: "agent/child-one".into(),
        base_sha: "abc123".into(),
        asked_ms: 2_000,
        answered_ms: None,
        decision: None,
        failure: failure.into(),
        reasons: Vec::new(),
        files: Vec::new(),
        commands: Vec::new(),
    }
}

fn queue_frame(
    entries: Vec<letibot_sessionlog::event::MergeEntry>,
    reviews: Vec<letibot_sessionlog::event::MergeReview>,
) -> ServerFrame {
    ServerFrame::MergeQueue { entries, reviews }
}

mod asks;
mod attention;
mod commands;
mod composer;
mod decisions;
mod detected;
mod editor;
mod jobs;
mod misc;
mod notes;
mod pane;
mod queue;
mod render;
mod screen;
mod scroll;
mod session;
mod settings;
mod standing;
mod subagents;
mod todos;
mod tools;
mod transcript;
mod turn;
mod window;
